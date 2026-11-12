//! Middleware and routing over one shared [`ParsedRequest`].
//!
//! The target is decoded once when the [`ParsedRequest`] is built. Middleware
//! and the router both receive that same value, so an authorization check and
//! the route it guards can never disagree about what the path or query meant.
//! Middleware sees only decoded data and has no access to a re-decoding API.

use crate::{
    Request,
    connection::Response,
    router::{Resolution, RouteError, RouteMatch, Router},
    target::{Target, TargetError, TargetLimits},
};

/// A request with its single decoded target interpretation.
#[derive(Debug)]
pub struct ParsedRequest {
    request: Request,
    target: Target,
}
impl ParsedRequest {
    /// On a target error the request is returned to its owner.
    pub fn new(request: Request, limits: TargetLimits) -> Result<Self, (Request, TargetError)> {
        match Target::parse(&request.head.target, limits) {
            Ok(target) => Ok(Self { request, target }),
            Err(error) => Err((request, error)),
        }
    }
    pub fn request(&self) -> &Request {
        &self.request
    }
    pub fn target(&self) -> &Target {
        &self.target
    }
    pub fn into_parts(self) -> (Request, Target) {
        (self.request, self.target)
    }
}

pub enum Flow {
    Continue,
    /// Short-circuit with this response; later middleware and routing are skipped.
    Respond(Response),
}

pub trait Middleware {
    fn inspect(&self, request: &ParsedRequest) -> Flow;
}

#[derive(Debug)]
pub enum Dispatch<'a, T> {
    Middleware { index: usize, response: Response },
    Route(RouteMatch<'a, T>),
    MethodNotAllowed(Vec<&'a str>),
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceError {
    MiddlewareLimit,
}

/// A router plus a bounded ordered middleware chain.
pub struct Service<T> {
    router: Router<T>,
    middleware: Vec<Box<dyn Middleware>>,
    max_middleware: usize,
}
impl<T> Service<T> {
    pub fn new(router: Router<T>, max_middleware: usize) -> Self {
        Self {
            router,
            middleware: Vec::new(),
            max_middleware,
        }
    }
    pub fn router(&self) -> &Router<T> {
        &self.router
    }
    pub fn push(&mut self, middleware: Box<dyn Middleware>) -> Result<(), ServiceError> {
        if self.middleware.len() >= self.max_middleware {
            return Err(ServiceError::MiddlewareLimit);
        }
        self.middleware.push(middleware);
        Ok(())
    }
    /// Run middleware in order, then route, all over the same decoded target.
    pub fn dispatch<'a>(
        &'a self,
        request: &'a ParsedRequest,
    ) -> Result<Dispatch<'a, T>, RouteError> {
        for (index, middleware) in self.middleware.iter().enumerate() {
            if let Flow::Respond(response) = middleware.inspect(request) {
                return Ok(Dispatch::Middleware { index, response });
            }
        }
        Ok(
            match self
                .router
                .resolve(&request.request.head.method, &request.target)?
            {
                Resolution::Matched(found) => Dispatch::Route(found),
                Resolution::MethodNotAllowed(allowed) => Dispatch::MethodNotAllowed(allowed),
                Resolution::NotFound => Dispatch::NotFound,
            },
        )
    }
}

/// A deterministic 405 with an `Allow` field listing `allowed` in order.
pub fn method_not_allowed(allowed: &[&str]) -> Response {
    Response::new(405, Vec::new()).header("Allow", allowed.join(", ").as_bytes())
}
