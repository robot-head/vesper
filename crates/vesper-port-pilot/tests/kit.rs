//! API tests of the pilot's export ABI v2 library (`vesper/kit.ves`, #898).
//! They use only the public API, so they run unchanged against the Rust
//! build and, with `--features vesper-port`, against the Vesper build. Each
//! exported form has its own test, which the divergent build must fail.
use vesper_port_pilot::kit::patterns::*;
use vesper_port_pilot::kit::*;

#[test]
fn sequence_results() {
    assert_eq!(squares(0), Vec::<u64>::new());
    assert_eq!(squares(5), [0, 1, 4, 9, 16]);
    assert_eq!(squares(1000).len(), 1000);
    assert_eq!(digits(0), [0]);
    assert_eq!(digits(9071), [1, 7, 0, 9]);
    assert_eq!(digits(u64::MAX).len(), 20);
    assert_eq!(countdown(3), [3, 2, 1, 0]);
    assert_eq!(countdown(-1), Vec::<i32>::new());
    let mut xs = vec![7];
    push_squares(&mut xs, 4);
    assert_eq!(xs, [7, 0, 1, 4, 9]);
    push_squares(&mut xs, 0);
    assert_eq!(xs.len(), 5);
    let mut grown = Vec::new();
    push_squares(&mut grown, 300);
    assert_eq!(grown[299], 299 * 299);
    let mut x = u64::MAX;
    bump(&mut x, 2);
    assert_eq!(x, 1);
}

#[test]
fn slice_parameters() {
    assert_eq!(sum(&[]), 0);
    assert_eq!(sum(&[1, 2, 3, u64::MAX]), 5);
    assert_eq!(count_digits(b"a1b22c333:"), 6);
    assert_eq!(count_digits(b""), 0);
    let mut xs = [1, -2, 3, i32::MAX];
    scale(&mut xs, -3);
    assert_eq!(xs, [-3, 6, -9, i32::MAX.wrapping_mul(-3)]);
    let mut empty: [i32; 0] = [];
    scale(&mut empty, 5);
}

#[test]
fn owned_handles() {
    let mut c = Counter::new();
    assert_eq!(c.total(), 0);
    c.record(5);
    c.record(7);
    assert_eq!(c.total(), 12);
    assert_eq!(c.hits(), [5, 7]);
    c.reset();
    assert_eq!(c.total(), 0);
    assert!(c.hits().is_empty());
    for v in 0..100 {
        c.record(v);
    }
    assert_eq!(c.total(), 4950);
    assert_eq!(c.hits().len(), 100);
    // Handles move between threads and drop there.
    let total = std::thread::spawn(move || c.total()).join().unwrap();
    assert_eq!(total, 4950);
}

#[test]
fn linear_handles() {
    let t = Ticket::open(41);
    assert_eq!(t.close(), 41);
    let tickets: Vec<Ticket> = (0..10).map(Ticket::open).collect();
    let ids: Vec<u64> = tickets.into_iter().map(Ticket::close).collect();
    assert_eq!(ids, (0..10).collect::<Vec<_>>());
}

#[test]
fn recursive_enum_handles() {
    assert_eq!(tree_shape(&Tree::build(0)), TreeNode::Leaf(1));
    let leaf = |v| Box::new(TreeNode::Leaf(v));
    assert_eq!(
        tree_shape(&Tree::build(1)),
        TreeNode::Node(leaf(2), leaf(3))
    );
    assert_eq!(Tree::build(1).sum(), 5);
    // Leaves 8..16.
    assert_eq!(Tree::build(3).sum(), (8..16).sum::<u64>());
    let big = Tree::build(10);
    let shape = tree_shape(&big);
    drop(big);
    let mut leaves = 0;
    let mut stack = vec![&shape];
    while let Some(n) = stack.pop() {
        match n {
            TreeNode::Leaf(_) => leaves += 1,
            TreeNode::Node(l, r) => stack.extend([&**l, &**r]),
        }
    }
    assert_eq!(leaves, 1024);
}

#[test]
fn generic_instances() {
    assert_eq!(largest_u64(&[], 3), 3);
    assert_eq!(largest_u64(&[4, 9, 2], 3), 9);
    assert_eq!(largest_u64(&[u64::MAX, 1], 0), u64::MAX);
    assert_eq!(largest_i64(&[-4, -9, -2], i64::MIN), -2);
    assert_eq!(largest_i64(&[-4], 0), 0);
}

#[test]
fn vsys_imports() {
    assert_eq!(stamp(5), 5 + shim::TICKS);
    assert_eq!(stamp(u64::MAX), shim::TICKS - 1);
}

#[test]
fn pattern_token_slab() {
    let mut slab = TokenSlab::new();
    slab.push(String::from("a"));
    slab.push(String::from("b"));
    assert_eq!(slab.pop().as_deref(), Some("a"));
    slab.push(String::from("c"));
    assert_eq!(slab.pop().as_deref(), Some("b"));
    assert_eq!(slab.pop().as_deref(), Some("c"));
    assert_eq!(slab.pop(), None);
    for i in 0..1000 {
        slab.push(i.to_string());
    }
    assert_eq!((0..1000).filter_map(|_| slab.pop()).count(), 1000);
}

#[test]
fn pattern_sink_closure() {
    let mut words = Vec::new();
    for_each_word(b"  the quick  brown fox", |w| {
        words.push(String::from_utf8(w.to_vec()).unwrap())
    });
    assert_eq!(words, ["the", "quick", "brown", "fox"]);
    let mut n = 0;
    for_each_word(b"   ", |_| n += 1);
    assert_eq!(n, 0);
}

struct Fixed(u64);
impl Clock for Fixed {
    fn now(&self) -> u64 {
        self.0
    }
}

#[derive(Default)]
struct Log(Vec<String>);
impl Transport for Log {
    fn send(&mut self, to: u32, bytes: u64) {
        self.0.push(format!("send {to} {bytes}"));
    }
    fn wait(&mut self, ticks: u64) {
        self.0.push(format!("wait {ticks}"));
    }
}

#[test]
fn pattern_trait_as_value_and_plan() {
    assert!(is_expired(&Fixed(10), 10));
    assert!(!is_expired(&Fixed(9), 10));
    let mut log = Log::default();
    broadcast(&mut log, 2, 64, 5);
    assert_eq!(log.0, ["send 0 64", "wait 5", "send 1 64", "wait 5"]);
    assert_eq!(
        plan_sends(1, 8, 2),
        [Command::Send { to: 0, bytes: 8 }, Command::Wait(2)]
    );
}

#[test]
fn pattern_errors_through_fail() {
    assert_eq!(mean(&[2, 4, 9]), Ok(5));
    assert_eq!(mean(&[]), Err(KitError::Empty));
    assert_eq!(mean(&[999_999, 2]), Err(KitError::TooLarge(1_000_001)));
    assert_eq!(mean_or_zero(&[]), Ok(0));
    let e: Box<dyn std::error::Error> = Box::new(mean(&[]).unwrap_err());
    assert_eq!(e.to_string(), "no values");
}

/// The borrowed views exist only on the Vesper build.
#[cfg(feature = "vesper-port")]
#[test]
fn borrowed_views_read_in_place() {
    let mut c = Counter::new();
    c.record(3);
    c.record(4);
    let view = c.as_ref();
    assert_eq!(view.total(), 7);
    assert_eq!(view.hits().to_vec(), [3, 4]);
    assert_eq!(view.hits().get(1), Some(4));
    assert_eq!(view.hits().get(2), None);
    let t = Tree::build(1);
    match t.as_ref().view() {
        TreeView::Node(l, _) => assert!(matches!(l.view(), TreeView::Leaf(2))),
        _ => panic!("a node"),
    }
    assert_eq!(Ticket::open(9).as_ref().id(), 9);
}
