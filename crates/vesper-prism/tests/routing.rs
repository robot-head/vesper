use vesper_prism::{
    Decoder, Limits,
    connection::{Connection, ConnectionLimits, RequestEvent, Response},
    router::{Resolution, RouteError, Router, RouterLimits},
    service::{Dispatch, Flow, Middleware, ParsedRequest, Service, method_not_allowed},
    target::{DuplicateQueryKey, Target, TargetError, TargetLimits, Utf8Policy},
};

type Captures = Vec<(String, Vec<u8>)>;

fn target(raw: &str) -> Result<Target, TargetError> {
    Target::parse(raw, TargetLimits::default())
}
fn segments(raw: &str) -> Vec<Vec<u8>> {
    target(raw)
        .unwrap()
        .origin()
        .unwrap()
        .segments()
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect()
}
fn router(routes: &[(&str, &str)]) -> Router<&'static str> {
    let mut router = Router::new(RouterLimits::default()).unwrap();
    for (method, pattern) in routes {
        let name: &'static str = Box::leak(format!("{method} {pattern}").into_boxed_str());
        router.insert(method, pattern, name).unwrap();
    }
    router
}
fn resolve(router: &Router<&'static str>, method: &str, raw: &str) -> Option<(String, Captures)> {
    let target = target(raw).unwrap();
    match router.resolve(method, &target).unwrap() {
        Resolution::Matched(found) => Some((
            (*found.value).to_owned(),
            found
                .params()
                .iter()
                .map(|(name, segment)| ((*name).to_owned(), segment.as_bytes().to_vec()))
                .collect(),
        )),
        _ => None,
    }
}

#[test]
fn path_segments_trailing_slash_and_plus_are_explicit() {
    assert!(segments("/").is_empty());
    assert!(!target("/").unwrap().origin().unwrap().trailing_slash());
    assert_eq!(segments("/a/b+c"), [b"a".to_vec(), b"b+c".to_vec()]);
    assert!(target("/a/").unwrap().origin().unwrap().trailing_slash());
    assert_eq!(segments("/a/"), [b"a".to_vec()]);
    // Escapes of unreserved bytes are the same data as the literal byte.
    assert_eq!(segments("/%61%2Bb"), [b"a+b".to_vec()]);
    assert_eq!(segments("/caf%C3%A9"), ["café".as_bytes().to_vec()]);
    // Encoded `?` and `#` are segment data, not query or fragment delimiters.
    assert_eq!(segments("/a%3Fb%23c"), [b"a?b#c".to_vec()]);
    assert!(target("/a").unwrap().origin().unwrap().query().is_none());
    assert_eq!(target("*").unwrap(), Target::Asterisk);
}

#[test]
fn separators_nul_dots_and_empty_segments_are_rejected() {
    for (raw, error) in [
        ("/a%2Fb", TargetError::EncodedSlash),
        ("/a%2fb", TargetError::EncodedSlash),
        ("/a%5Cb", TargetError::EncodedBackslash),
        ("/a%00b", TargetError::EncodedNul),
        ("/a?k=%00", TargetError::EncodedNul),
        ("/a?%00=v", TargetError::EncodedNul),
        ("/.", TargetError::DotSegment),
        ("/a/../b", TargetError::DotSegment),
        ("/%2e", TargetError::DotSegment),
        ("/a/%2E%2e/b", TargetError::DotSegment),
        ("/.%2e/", TargetError::DotSegment),
        ("/a//b", TargetError::EmptySegment),
        ("/a//", TargetError::EmptySegment),
        ("//a", TargetError::InvalidSyntax),
        ("/a\\b", TargetError::InvalidSyntax),
        ("/a#frag", TargetError::InvalidSyntax),
        ("/é", TargetError::InvalidSyntax),
        ("a", TargetError::InvalidSyntax),
        ("/%zz", TargetError::InvalidEscape),
        ("/%4", TargetError::InvalidEscape),
        ("/a?b=%g0", TargetError::InvalidEscape),
    ] {
        assert_eq!(target(raw), Err(error), "{raw}");
    }
    // Dots that are not whole segments are ordinary data.
    assert_eq!(
        segments("/a.b/.../..c"),
        [b"a.b".to_vec(), b"...".to_vec(), b"..c".to_vec()]
    );
}

#[test]
fn utf8_policy_is_explicit_for_paths_and_queries() {
    assert_eq!(target("/%FF"), Err(TargetError::InvalidUtf8));
    assert_eq!(target("/a?k=%C3"), Err(TargetError::InvalidUtf8));
    assert_eq!(target("/%C0%AF"), Err(TargetError::InvalidUtf8)); // overlong `/`
    let bytes = TargetLimits {
        utf8: Utf8Policy::AllowBytes,
        ..TargetLimits::default()
    };
    let parsed = Target::parse("/%FF%00ok", bytes);
    assert_eq!(parsed, Err(TargetError::EncodedNul));
    let parsed = Target::parse("/%FFok?k=%FE", bytes).unwrap();
    let origin = parsed.origin().unwrap();
    assert_eq!(origin.segments()[0].as_bytes(), b"\xffok");
    assert_eq!(origin.segments()[0].as_str(), None);
    assert_eq!(
        origin.query().unwrap().entries()[0].value.as_deref(),
        Some(&b"\xfe"[..])
    );
}

#[test]
fn double_decode_probes_stay_single_decoded() {
    // `%252e%252e` is the literal text `%2e%2e`, never a dot segment.
    assert_eq!(
        segments("/files/%252e%252e/etc"),
        [b"files".to_vec(), b"%2e%2e".to_vec(), b"etc".to_vec()]
    );
    assert_eq!(segments("/%252F"), [b"%2F".to_vec()]);
    let query = target("/?next=%2541").unwrap();
    let entry = query
        .origin()
        .unwrap()
        .query()
        .unwrap()
        .unique(b"next")
        .unwrap()
        .unwrap();
    assert_eq!(entry.value.as_deref(), Some(&b"%41"[..]));
    // Routing compares the singly decoded bytes: `%2561` is `%61`, not `a`.
    let router = router(&[("GET", "/a"), ("GET", "/:p")]);
    assert_eq!(resolve(&router, "GET", "/%61").unwrap().0, "GET /a");
    let (route, params) = resolve(&router, "GET", "/%2561").unwrap();
    assert_eq!(route, "GET /:p");
    assert_eq!(params, [("p".to_owned(), b"%61".to_vec())]);
    // A literal route may not be written with escapes, avoiding a second decode.
    let mut strict = Router::<()>::new(RouterLimits::default()).unwrap();
    assert_eq!(
        strict.insert("GET", "/%61", ()),
        Err(RouteError::InvalidPattern)
    );
}

#[test]
fn query_entries_plus_duplicates_and_empty_forms_are_defined() {
    let parsed = target("/s?q=a+b%2Bc&flag&empty=&q=second&k%3D=v%26w").unwrap();
    let query = parsed.origin().unwrap().query().unwrap();
    let entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = query
        .entries()
        .iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect();
    assert_eq!(
        entries,
        [
            (b"q".to_vec(), Some(b"a b+c".to_vec())),
            (b"flag".to_vec(), None),
            (b"empty".to_vec(), Some(Vec::new())),
            (b"q".to_vec(), Some(b"second".to_vec())),
            (b"k=".to_vec(), Some(b"v&w".to_vec())),
        ]
    );
    assert_eq!(query.unique(b"q"), Err(DuplicateQueryKey));
    assert_eq!(query.all(b"q").count(), 2);
    assert_eq!(query.unique(b"flag").unwrap().unwrap().value, None);
    assert_eq!(query.unique(b"missing"), Ok(None));
    assert!(
        target("/s?")
            .unwrap()
            .origin()
            .unwrap()
            .query()
            .unwrap()
            .entries()
            .is_empty()
    );
    for (raw, error) in [
        ("/s?a&&b", TargetError::EmptyQueryEntry),
        ("/s?a&", TargetError::EmptyQueryEntry),
        ("/s?&a", TargetError::EmptyQueryEntry),
        ("/s?=v", TargetError::EmptyQueryKey),
    ] {
        assert_eq!(target(raw), Err(error), "{raw}");
    }
    // `=` after the first one is value data.
    let parsed = target("/s?a=b=c").unwrap();
    let value = &parsed.origin().unwrap().query().unwrap().entries()[0].value;
    assert_eq!(value.as_deref(), Some(&b"b=c"[..]));
}

#[test]
fn target_limits_are_checked_before_allocation() {
    let limits = TargetLimits {
        max_target_bytes: 16,
        max_segments: 3,
        max_query_entries: 2,
        utf8: Utf8Policy::Require,
    };
    assert!(Target::parse("/a/b/c", limits).is_ok());
    assert!(Target::parse("/a/b/c/", limits).is_ok());
    assert_eq!(
        Target::parse("/a/b/c/d", limits),
        Err(TargetError::TooManySegments)
    );
    assert!(Target::parse("/?a&b", limits).is_ok());
    assert_eq!(
        Target::parse("/?a&b&c", limits),
        Err(TargetError::TooManyQueryEntries)
    );
    assert_eq!(
        Target::parse("/aaaaaaaaaaaaaaaa", limits),
        Err(TargetError::TooLong)
    );
}

#[test]
fn static_segments_beat_parameters_independent_of_insertion_order() {
    let forward = router(&[
        ("GET", "/users/new"),
        ("GET", "/users/:id"),
        ("GET", "/users/:id/posts"),
        ("GET", "/users/new/posts"),
    ]);
    let reverse = router(&[
        ("GET", "/users/new/posts"),
        ("GET", "/users/:id/posts"),
        ("GET", "/users/:id"),
        ("GET", "/users/new"),
    ]);
    for router in [&forward, &reverse] {
        assert_eq!(
            resolve(router, "GET", "/users/new").unwrap().0,
            "GET /users/new"
        );
        let (route, params) = resolve(router, "GET", "/users/42").unwrap();
        assert_eq!(route, "GET /users/:id");
        assert_eq!(params, [("id".to_owned(), b"42".to_vec())]);
        assert_eq!(
            resolve(router, "GET", "/users/new/posts").unwrap().0,
            "GET /users/new/posts"
        );
        assert_eq!(
            resolve(router, "GET", "/users/%6Eew").unwrap().0,
            "GET /users/new"
        );
        assert!(resolve(router, "GET", "/users").is_none());
        assert!(resolve(router, "GET", "/users/42/").is_none());
    }
}

#[test]
fn backtracking_falls_back_to_parameter_when_static_branch_dead_ends() {
    let router = router(&[("GET", "/a/b/d"), ("GET", "/a/:x/c"), ("GET", "/:y/b/c/")]);
    let (route, params) = resolve(&router, "GET", "/a/b/c").unwrap();
    assert_eq!(route, "GET /a/:x/c");
    assert_eq!(params, [("x".to_owned(), b"b".to_vec())]);
    // Trailing slash is significant: only the third route has one.
    let (route, params) = resolve(&router, "GET", "/a/b/c/").unwrap();
    assert_eq!(route, "GET /:y/b/c/");
    assert_eq!(params, [("y".to_owned(), b"a".to_vec())]);
}

#[test]
fn ambiguous_duplicate_and_malformed_routes_are_rejected_atomically() {
    let mut router = Router::new(RouterLimits::default()).unwrap();
    router.insert("GET", "/users/:id", 1).unwrap();
    assert_eq!(
        router.insert("GET", "/users/:name/posts", 2),
        Err(RouteError::AmbiguousParameter {
            existing: "id".to_owned(),
            new: "name".to_owned()
        })
    );
    assert_eq!(
        router.insert("GET", "/users/:id", 3),
        Err(RouteError::DuplicateRoute)
    );
    assert_eq!(
        router.insert("GET", "/a/:x/:x", 4),
        Err(RouteError::DuplicateParameterName)
    );
    for pattern in [
        "", "users", "//", "/a//b", "/a/./b", "/..", "/a%2Fb", "/a?b", "/:", "/:1x", "/:a-b",
        "/a\\b",
    ] {
        assert!(router.insert("GET", pattern, 5).is_err(), "{pattern}");
    }
    assert_eq!(
        router.insert("GE T", "/x", 6),
        Err(RouteError::InvalidMethod)
    );
    // Failed inserts left no partial nodes: the same shape registers cleanly.
    assert_eq!(router.len(), 1);
    router.insert("POST", "/users/:id", 7).unwrap();
    router.insert("GET", "/users/:id/", 8).unwrap();
    assert_eq!(router.len(), 3);
}

#[test]
fn methods_head_fallback_and_405_allow_list_are_deterministic() {
    let router = router(&[
        ("POST", "/items"),
        ("GET", "/items"),
        ("DELETE", "/items"),
        ("HEAD", "/h"),
        ("GET", "/h"),
    ]);
    let parsed = target("/items").unwrap();
    let Resolution::Matched(found) = router.resolve("HEAD", &parsed).unwrap() else {
        panic!()
    };
    assert!(found.head_fallback);
    assert_eq!(*found.value, "GET /items");
    let h = target("/h").unwrap();
    let Resolution::Matched(found) = router.resolve("HEAD", &h).unwrap() else {
        panic!()
    };
    assert!(!found.head_fallback);
    assert_eq!(*found.value, "HEAD /h");
    let Resolution::MethodNotAllowed(allowed) = router.resolve("PUT", &parsed).unwrap() else {
        panic!()
    };
    assert_eq!(allowed, ["DELETE", "GET", "HEAD", "POST"]);
    assert_eq!(
        method_not_allowed(&allowed).headers,
        [("Allow".to_owned(), b"DELETE, GET, HEAD, POST".to_vec())]
    );
    // Methods are case-sensitive tokens.
    assert!(matches!(
        router.resolve("get", &parsed).unwrap(),
        Resolution::MethodNotAllowed(_)
    ));
    // A higher-precedence path is not abandoned for a parameter route's method.
    let router = self::router(&[("GET", "/u/new"), ("POST", "/u/:id")]);
    let new = target("/u/new").unwrap();
    let Resolution::MethodNotAllowed(allowed) = router.resolve("POST", &new).unwrap() else {
        panic!()
    };
    assert_eq!(allowed, ["GET", "HEAD"]);
    assert!(matches!(
        router.resolve("GET", &Target::Asterisk).unwrap(),
        Resolution::NotFound
    ));
}

#[test]
fn route_and_match_work_limits_fail_closed() {
    let limits = RouterLimits {
        max_routes: 2,
        max_route_segments: 3,
        max_match_work: 5,
    };
    let mut router = Router::new(limits).unwrap();
    assert_eq!(
        router.insert("GET", "/a/b/c/d", ()),
        Err(RouteError::SegmentLimit)
    );
    // Static prefixes that all dead-end force backtracking through each param.
    router.insert("GET", "/a/a/x", ()).unwrap();
    router.insert("GET", "/:p/:q/:r", ()).unwrap();
    assert_eq!(router.insert("GET", "/z", ()), Err(RouteError::RouteLimit));
    assert!(matches!(
        router.resolve("GET", &target("/b/c/d").unwrap()),
        Ok(Resolution::Matched(_))
    ));
    assert_eq!(
        router
            .resolve("GET", &target("/a/a/y").unwrap())
            .unwrap_err(),
        RouteError::WorkLimit
    );
    assert!(
        Router::<()>::new(RouterLimits {
            max_routes: 0,
            ..limits
        })
        .is_err()
    );
}

struct DenyAdmin;
impl Middleware for DenyAdmin {
    fn inspect(&self, request: &ParsedRequest) -> Flow {
        let first = request.target().origin().and_then(|o| o.segments().first());
        if first.is_some_and(|s| s.as_bytes() == b"admin") {
            Flow::Respond(Response::new(403, Vec::new()))
        } else {
            Flow::Continue
        }
    }
}

fn decode(raw: &[u8]) -> vesper_prism::Request {
    let mut decoder = Decoder::new(Limits::default()).unwrap();
    decoder.push(raw).unwrap();
    decoder.next_request().unwrap().unwrap().request
}

#[test]
fn middleware_and_router_share_one_decoded_target() {
    let mut service = Service::new(router(&[("GET", "/admin/panel"), ("GET", "/:page")]), 2);
    service.push(Box::new(DenyAdmin)).unwrap();
    service.push(Box::new(DenyAdmin)).unwrap();
    assert!(service.push(Box::new(DenyAdmin)).is_err());
    // An escaped spelling cannot bypass the guard: both see the same `admin`.
    for raw in ["/admin/panel", "/%61dmin/panel", "/%61%64%6D%69%6E/panel"] {
        let request = decode(format!("GET {raw} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes());
        let parsed = ParsedRequest::new(request, TargetLimits::default()).unwrap();
        let Dispatch::Middleware { index, response } = service.dispatch(&parsed).unwrap() else {
            panic!("{raw} bypassed middleware")
        };
        assert_eq!((index, response.status), (0, 403));
        assert_eq!(parsed.request().head.target, raw, "raw target is preserved");
    }
    // A singly decoded `%2561dmin` is data `%61dmin`, which is not guarded and
    // routes to the parameter route with that exact value.
    let request = decode(b"GET /%2561dmin HTTP/1.1\r\nHost: x\r\n\r\n");
    let parsed = ParsedRequest::new(request, TargetLimits::default()).unwrap();
    let Dispatch::Route(found) = service.dispatch(&parsed).unwrap() else {
        panic!()
    };
    assert_eq!(found.param("page").unwrap().as_bytes(), b"%61dmin");
    // Invalid targets return the request to its owner.
    let request = decode(b"GET /a%2Fb HTTP/1.1\r\nHost: x\r\n\r\n");
    let (request, error) = ParsedRequest::new(request, TargetLimits::default()).unwrap_err();
    assert_eq!(error, TargetError::EncodedSlash);
    assert_eq!(request.head.target, "/a%2Fb");
}

#[test]
fn routed_head_through_a_connection_suppresses_the_get_body() {
    let service = Service::new(router(&[("GET", "/doc")]), 0);
    let mut connection = Connection::new(ConnectionLimits::default()).unwrap();
    let input = b"HEAD /doc HTTP/1.1\r\nHost: x\r\n\r\nGET /doc HTTP/1.1\r\nHost: x\r\n\r\nPUT /doc HTTP/1.1\r\nHost: x\r\n\r\nGET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    assert_eq!(connection.receive(input).unwrap(), input.len());
    while let RequestEvent::Request(inbound) = connection.poll_request() {
        let parsed = ParsedRequest::new(inbound.request, TargetLimits::default()).unwrap();
        let response = match service.dispatch(&parsed).unwrap() {
            Dispatch::Route(_) => Response::new(200, b"body".to_vec()),
            Dispatch::MethodNotAllowed(allowed) => method_not_allowed(&allowed),
            Dispatch::NotFound => Response::new(404, Vec::new()),
            Dispatch::Middleware { response, .. } => response,
        };
        connection.submit(inbound.id, response).unwrap();
    }
    let mut out = Vec::new();
    while !connection.pending_output().is_empty() {
        connection.write_to(&mut out).unwrap();
    }
    assert_eq!(
        out,
        [
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nbody",
            b"HTTP/1.1 405 Method Not Allowed\r\nallow: GET, HEAD\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n",
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ]
        .concat()
    );
}
