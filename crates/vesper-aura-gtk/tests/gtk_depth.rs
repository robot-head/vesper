//! Wave-eight acceptance tests for the GTK backend under Xvfb: layouts, CSS,
//! virtual lists, close wiring, the GLib/Pulse integration, AT-SPI checks and
//! golden screenshots.
//!
//! `harness = false`: GTK is single-threaded, so every case runs in order on
//! the main thread. Run with
//! `dbus-run-session -- xvfb-run -a cargo test -p vesper-aura-gtk --features gtk`
//! (the session bus is needed for AT-SPI; see .github/workflows/aura-gtk.yml).
//! `VESPER_REQUIRE_ATSPI=1` turns a missing accessibility bus into a failure;
//! `VESPER_UPDATE_GOLDENS=1` rewrites the golden PNGs.

use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::Write,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use vesper_aura::*;
use vesper_aura_gtk::{
    atspi::{self, AtspiClient, State},
    dispatch, gtk4 as gtk,
    image::{self, RgbaImage, Tolerance},
    mainloop::{self, CoSchedule, PulseOutput},
    *,
};
use vesper_pulse::scheduler::{Event as PulseEvent, Operation, Request, Resume, Scheduler, Step};

const APP_NAME: &str = "vesper-aura-gtk-depth";
const SETTLE: Duration = Duration::from_secs(10);

fn text(key: u64, s: &str) -> Node {
    Node::new(Key(key), Kind::Text).property(Property::Text, Value::Text(s.into()))
}
fn button(key: u64, s: &str) -> Node {
    Node::new(Key(key), Kind::Button)
        .property(Property::Text, Value::Text(s.into()))
        .on(Event::Click, HandlerKey(key))
}
fn input(key: u64, placeholder: &str) -> Node {
    Node::new(Key(key), Kind::Input)
        .property(Property::Text, Value::Text(placeholder.into()))
        .on(Event::Changed, HandlerKey(key))
}
fn container(key: u64, children: &[u64]) -> Node {
    Node::new(Key(key), Kind::Container).children(children.iter().map(|k| Key(*k)))
}

fn host(thread: &UiThread) -> GtkHost {
    GtkHost::new(thread, Limits::default()).expect("GTK host")
}

fn render(host: &mut GtkHost, thread: &UiThread, root: u64, nodes: Vec<Node>) {
    host.render(thread, Some(ViewTree::new(Key(root), nodes)))
        .expect("render");
    host.backend(thread).unwrap().audit(thread).unwrap();
}

fn native(host: &GtkHost, thread: &UiThread) -> NativeNode {
    host.backend(thread)
        .unwrap()
        .native_tree(thread)
        .unwrap()
        .expect("mounted")
}

fn widget(host: &GtkHost, thread: &UiThread, key: u64) -> gtk::Widget {
    let id = host.widget(Key(key)).expect("mounted key");
    host.backend(thread)
        .unwrap()
        .native_widget(thread, id)
        .unwrap()
}

fn settle(host: &GtkHost, thread: &UiThread) {
    host.backend(thread)
        .unwrap()
        .settle(thread, 3, SETTLE)
        .expect("frames under Xvfb");
}

fn shutdown(host: GtkHost, thread: &UiThread) {
    let report = host.shutdown(thread).expect("shutdown");
    assert_eq!(report.still_alive, 0, "{report:?}");
}

/// Top-left of `w` in `root` coordinates.
fn origin(w: &gtk::Widget, root: &gtk::Widget) -> (f32, f32) {
    let b = w.compute_bounds(root).expect("bounds");
    (b.x(), b.y())
}

fn grid(columns: u16) -> Layout {
    Layout::Grid {
        columns,
        row_spacing: 3,
        column_spacing: 5,
    }
}

fn layouts_follow_keys(thread: &UiThread) {
    let mut h = host(thread);
    h.set_style(thread, Key(0), Style::default().layout(grid(2)))
        .unwrap();
    let nodes = |order: &[u64]| {
        let mut v = vec![container(0, order), container(9, &[7, 8])];
        v.extend((1..=5).map(|k| button(k, &format!("b{k}"))));
        v.push(text(7, "left"));
        v.push(text(8, "right"));
        v
    };
    render(&mut h, thread, 0, nodes(&[1, 2, 3, 4, 5, 9]));
    let tree = native(&h, thread);
    assert_eq!(
        tree.layout,
        Some(NativeLayout::Grid {
            row_spacing: 3,
            column_spacing: 5
        })
    );
    let cells: Vec<_> = tree.children.iter().map(|c| c.grid_cell).collect();
    assert_eq!(
        cells,
        [(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)].map(Some)
    );
    // Real geometry after GTK allocates: column 1 is right of column 0 by at
    // least the column spacing, row 1 is below row 0.
    settle(&h, thread);
    let root = widget(&h, thread, 0);
    let (b1, b2, b3) = (
        widget(&h, thread, 1),
        widget(&h, thread, 2),
        widget(&h, thread, 3),
    );
    let (p1, p2, p3) = (origin(&b1, &root), origin(&b2, &root), origin(&b3, &root));
    assert_eq!(p1.1, p2.1);
    assert!(p2.0 >= p1.0 + b1.width() as f32 + 5.0, "{p1:?} {p2:?}");
    assert_eq!(p1.0, p3.0);
    assert!(p3.1 >= p1.1 + b1.height() as f32 + 3.0, "{p1:?} {p3:?}");

    // Reordering keeps widgets and recomputes cells.
    render(&mut h, thread, 0, nodes(&[5, 4, 3, 2, 1, 9]));
    assert_eq!(widget(&h, thread, 1), b1);
    let tree = native(&h, thread);
    assert_eq!(tree.children[0].text.as_deref(), Some("b5"));
    assert_eq!(tree.children[4].grid_cell, Some((0, 2)));

    // A nested horizontal stack, then the root back to stacks.
    h.set_style(
        thread,
        Key(9),
        Style::default().layout(Layout::HStack { spacing: 6 }),
    )
    .unwrap();
    h.set_style(
        thread,
        Key(0),
        Style::default().layout(Layout::VStack { spacing: 2 }),
    )
    .unwrap();
    h.backend(thread).unwrap().audit(thread).unwrap();
    let tree = native(&h, thread);
    assert_eq!(
        tree.layout,
        Some(NativeLayout::Box {
            horizontal: false,
            spacing: 2
        })
    );
    assert!(tree.children.iter().all(|c| c.grid_cell.is_none()));
    assert_eq!(
        tree.children[5].layout,
        Some(NativeLayout::Box {
            horizontal: true,
            spacing: 6
        })
    );
    settle(&h, thread);
    let (l, r) = (widget(&h, thread, 7), widget(&h, thread, 8));
    let (pl, pr) = (origin(&l, &root), origin(&r, &root));
    assert_eq!(pl.1, pr.1, "an HStack lays children out on one row");
    assert!(pr.0 >= pl.0 + l.width() as f32 + 6.0);

    // A layout on a node that becomes a non-container is rejected before
    // anything is committed.
    let before = native(&h, thread);
    let revision = h.session().revision();
    let mut bad = nodes(&[1, 2, 3, 4, 5]);
    bad[1] = text(9, "no longer a container");
    match h.render(thread, Some(ViewTree::new(Key(0), bad))) {
        Err(HostError::Style { key: Key(9), .. }) => {}
        other => panic!("expected a style error, got {other:?}"),
    }
    assert_eq!(h.session().revision(), revision);
    assert_eq!(native(&h, thread), before);
    assert!(matches!(
        h.set_style(thread, Key(1), Style::default().layout(grid(2))),
        Err(HostError::Style { .. })
    ));
    assert!(matches!(
        h.set_style(thread, Key(0), Style::default().layout(grid(0))),
        Err(HostError::Backend(GtkError::Unsupported(_)))
    ));
    drop((root, b1, b2, b3, l, r));
    shutdown(h, thread);
}

fn css_classes_and_stylesheets(thread: &UiThread) {
    let mut h = host(thread);
    render(
        &mut h,
        thread,
        0,
        vec![container(0, &[1, 2]), button(1, "styled"), text(2, "plain")],
    );
    let b = h.backend_mut(thread).unwrap();
    let error = b
        .load_css(thread, ".wide { min-width: 3px; } .broken { colour: red; }")
        .unwrap_err();
    assert!(
        matches!(&error, GtkError::Css(msgs) if !msgs.is_empty()),
        "{error:?}"
    );
    b.load_css(
        thread,
        ".wide { min-width: 177px; } .tall { min-height: 61px; }",
    )
    .unwrap();
    h.set_style(thread, Key(1), Style::default().class("wide").class("tall"))
        .unwrap();
    h.backend(thread).unwrap().audit(thread).unwrap();
    assert_eq!(native(&h, thread).children[0].css_classes, ["tall", "wide"]);
    let w = widget(&h, thread, 1);
    settle(&h, thread);
    let (base_w, _, _, _) = widget(&h, thread, 2).measure(gtk::Orientation::Horizontal, -1);
    let (min_w, _, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
    let (min_h, _, _, _) = w.measure(gtk::Orientation::Vertical, -1);
    println!("  styled button min {min_w}x{min_h}, plain label min width {base_w}");
    assert!(min_w >= 177 && min_h >= 61, "{min_w}x{min_h}");
    // Classes follow the key across reorder; changing them updates GTK.
    render(
        &mut h,
        thread,
        0,
        vec![container(0, &[2, 1]), button(1, "styled"), text(2, "plain")],
    );
    assert_eq!(native(&h, thread).children[1].css_classes, ["tall", "wide"]);
    h.set_style(thread, Key(1), Style::default().class("tall"))
        .unwrap();
    assert_eq!(native(&h, thread).children[1].css_classes, ["tall"]);
    settle(&h, thread);
    let (min_w, _, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
    assert!(min_w < 177, "the removed class no longer applies ({min_w})");
    assert!(matches!(
        h.set_style(thread, Key(1), Style::default().class("not a class")),
        Err(HostError::Backend(GtkError::Unsupported(_)))
    ));
    let b = h.backend_mut(thread).unwrap();
    assert!(matches!(
        b.load_css(thread, &" ".repeat(MAX_CSS_BYTES + 1)),
        Err(GtkError::Unsupported(_))
    ));
    b.clear_css(thread).unwrap();
    settle(&h, thread);
    let (min_h, _, _, _) = w.measure(gtk::Orientation::Vertical, -1);
    assert!(min_h < 61, "cleared stylesheet no longer applies ({min_h})");
    drop(w);
    shutdown(h, thread);
}

fn virtual_list_recycles_rows(thread: &UiThread) {
    const ROWS: u32 = 100_000;
    let mut h = host(thread);
    let window = h.backend(thread).unwrap().gtk_window(thread).unwrap();
    window.set_default_size(320, 360);
    let fetched = Rc::new(Cell::new(0u64));
    let counter = Rc::clone(&fetched);
    let rows: RowText = Rc::new(move |i| {
        counter.set(counter.get() + 1);
        format!("row {i}")
    });
    h.set_virtual_list(thread, Key(1), ROWS, Rc::clone(&rows))
        .unwrap();
    render(
        &mut h,
        thread,
        0,
        vec![container(0, &[2, 1]), text(2, "header"), container(1, &[])],
    );
    assert_eq!(native(&h, thread).children[1].virtual_rows, Some(ROWS));
    settle(&h, thread);
    let list = h.widget(Key(1)).unwrap();
    let b = h.backend(thread).unwrap();
    let stats = b.virtual_list_stats(thread, list).unwrap();
    let visible = b.visible_rows(thread, list).unwrap();
    println!("  initial {stats:?}, {} visible", visible.len());
    assert!(
        visible.first().map(String::as_str) == Some("row 0"),
        "{visible:?}"
    );
    // GTK's list view keeps a fixed overscan of roughly 200 rows around the
    // viewport (205 here for 16 visible); the bound is independent of ROWS.
    assert!(
        stats.row_widgets > 0 && stats.row_widgets <= 512,
        "{stats:?}"
    );
    assert!(stats.bound <= 512 && fetched.get() <= 1024);

    b.scroll_virtual_list(thread, list, ROWS - 1).unwrap();
    settle(&h, thread);
    let stats = b.virtual_list_stats(thread, list).unwrap();
    let visible = b.visible_rows(thread, list).unwrap();
    println!("  after scroll {stats:?}, last {:?}", visible.last());
    assert!(visible.iter().any(|r| r == "row 99999"), "{visible:?}");
    assert_eq!(stats.max_bound_index, Some(ROWS - 1));
    assert!(stats.row_widgets <= 512 && stats.bound <= 512, "{stats:?}");
    assert!(
        fetched.get() < 2_000,
        "rows are fetched lazily ({})",
        fetched.get()
    );
    assert!(matches!(
        b.scroll_virtual_list(thread, list, ROWS),
        Err(GtkError::Unsupported(_))
    ));

    // Row count changes go through the host; the audit sees them.
    h.set_virtual_list(thread, Key(1), 10, Rc::clone(&rows))
        .unwrap();
    h.backend(thread).unwrap().audit(thread).unwrap();
    assert_eq!(native(&h, thread).children[1].virtual_rows, Some(10));
    // Aura children may not enter a list container: rejected up front.
    let revision = h.session().revision();
    let err = h
        .render(
            thread,
            Some(ViewTree::new(
                Key(0),
                vec![
                    container(0, &[2, 1]),
                    text(2, "header"),
                    container(1, &[3]),
                    text(3, "x"),
                ],
            )),
        )
        .unwrap_err();
    assert!(
        matches!(err, HostError::Style { key: Key(1), .. }),
        "{err:?}"
    );
    assert_eq!(h.session().revision(), revision);
    // Unmount and remount the key: the list is re-attached to the new widget.
    render(
        &mut h,
        thread,
        0,
        vec![container(0, &[2]), text(2, "header")],
    );
    assert!(h.widget(Key(1)).is_none());
    render(
        &mut h,
        thread,
        0,
        vec![container(0, &[2, 1]), text(2, "header"), container(1, &[])],
    );
    let again = h.widget(Key(1)).unwrap();
    assert_ne!(again, list);
    assert_eq!(native(&h, thread).children[1].virtual_rows, Some(10));
    assert!(matches!(
        h.backend_mut(thread)
            .unwrap()
            .set_virtual_rows(thread, again, MAX_VIRTUAL_ROWS + 1),
        Err(GtkError::Unsupported(_))
    ));
    drop(window);
    shutdown(h, thread);
}

fn close_button_requests_close(thread: &UiThread) {
    let mut h = host(thread);
    render(&mut h, thread, 0, vec![text(0, "unsaved")]);
    settle(&h, thread);
    let window = h.backend(thread).unwrap().gtk_window(thread).unwrap();
    // The close request becomes a pending Session::request_close decision.
    window.close();
    let got = h.wait(thread, SETTLE).unwrap();
    let decision = got.close.expect("close decision");
    assert!(window.child().is_some(), "GTK did not close the window");
    // A second request while one is pending is coalesced.
    window.close();
    let got = h.wait(thread, Duration::from_millis(200)).unwrap();
    assert!(got.close.is_none());
    assert_eq!(got.close_coalesced, 1);
    assert_eq!(
        h.resolve_close(thread, decision, CloseAction::Veto)
            .unwrap(),
        CloseOutcome::Vetoed
    );
    assert!(window.child().is_some() && window.is_visible());
    // After a veto the next request yields a fresh decision; allow unmounts.
    window.close();
    let decision = h.wait(thread, SETTLE).unwrap().close.expect("decision");
    assert!(matches!(
        h.resolve_close(thread, decision, CloseAction::Allow)
            .unwrap(),
        CloseOutcome::Closed(_)
    ));
    assert!(window.child().is_none());
    window.close();
    let got = h.wait(thread, Duration::from_millis(200)).unwrap();
    assert!(got.close.is_none() && got.close_coalesced == 1);
    drop(window);
    shutdown(h, thread);
}

fn dispatcher_wakes_the_main_loop(thread: &UiThread) {
    let h = Rc::new(RefCell::new(host(thread)));
    render(&mut h.borrow_mut(), thread, 0, vec![text(0, "0")]);
    let (tx, rx) = dispatch::channel::<(u32, u32)>(64).unwrap();
    let received = Rc::new(RefCell::new(Vec::new()));
    let (sink, ui) = (Rc::clone(&received), Rc::clone(&h));
    let ui_thread = UiThread::claim_current();
    let task = mainloop::spawn_inbox(thread, rx, 16, move |batch| {
        let mut got = sink.borrow_mut();
        got.extend(batch);
        // Each batch drives an Aura update on the GTK thread.
        let n = got.len().to_string();
        ui.borrow_mut()
            .render(&ui_thread, Some(ViewTree::new(Key(0), vec![text(0, &n)])))
            .unwrap();
    })
    .unwrap();
    let senders: Vec<_> = (0..4u32)
        .map(|t| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for i in 0..250u32 {
                    let mut message = (t, i);
                    // The inbox is bounded (64): retry while it is full.
                    loop {
                        match tx.send(message) {
                            Ok(()) => break,
                            Err(dispatch::DispatchError::Full(m)) => {
                                message = m;
                                std::thread::sleep(Duration::from_micros(50));
                            }
                            Err(e) => panic!("{e:?}"),
                        }
                    }
                }
            })
        })
        .collect();
    mainloop::pump_until(thread, SETTLE, || received.borrow().len() == 1000).unwrap();
    for s in senders {
        s.join().unwrap();
    }
    assert_eq!(task.delivered(), 1000);
    for t in 0..4 {
        let mine: Vec<u32> = received
            .borrow()
            .iter()
            .filter(|(s, _)| *s == t)
            .map(|(_, i)| *i)
            .collect();
        assert_eq!(mine, (0..250).collect::<Vec<_>>(), "order of sender {t}");
    }
    assert_eq!(native(&h.borrow(), thread).text.as_deref(), Some("1000"));
    assert!(task.close().is_empty());
    assert!(matches!(
        tx.send((9, 9)),
        Err(dispatch::DispatchError::Closed(_))
    ));
    let h = Rc::try_unwrap(h).expect("sole owner").into_inner();
    shutdown(h, thread);
}

/// A Pulse continuation: either a pipe read or a CPU loop that yields.
enum Job {
    Read { file: Option<File>, tag: u8 },
    Spin { left: u32, tag: u8 },
}

impl Resume for Job {
    fn resume(self, event: PulseEvent) -> Step<Self> {
        match (self, event) {
            (
                Job::Read {
                    file: Some(file),
                    tag,
                },
                PulseEvent::Start,
            ) => Step::Io(Request {
                file,
                buffer: vec![0; 64],
                continuation: Job::Read { file: None, tag },
                operation: Operation::Read,
                offset: 0,
            }),
            (Job::Read { tag, .. }, PulseEvent::Io(io)) => match io.result {
                Ok(n) => {
                    let mut out = vec![tag];
                    out.extend_from_slice(&io.buffer[..n]);
                    Step::Complete(out)
                }
                Err(e) => Step::Failed(e.to_string()),
            },
            (Job::Spin { left: 0, tag }, _) => Step::Complete(vec![tag]),
            (Job::Spin { left, tag }, _) => Step::Yield(Job::Spin {
                left: left - 1,
                tag,
            }),
            (_, _) => Step::Failed("unexpected event".into()),
        }
    }
}

fn pipe() -> (File, File) {
    let (r, w) = std::io::pipe().expect("pipe");
    (
        File::from(std::os::fd::OwnedFd::from(r)),
        File::from(std::os::fd::OwnedFd::from(w)),
    )
}

fn pulse_co_schedules_with_gtk(thread: &UiThread) {
    let mut h = host(thread);
    render(&mut h, thread, 0, vec![button(0, "tap")]);
    settle(&h, thread);
    let (driver, report) =
        vesper_pulse::select::open::<Job>(64, vesper_pulse::select::Preference::Auto)
            .expect("a Pulse driver");
    println!("  Pulse driver: {}", report.selected.name());
    let mut scheduler = Scheduler::new(driver);
    let mut writers = Vec::new();
    for tag in 0..3u8 {
        let (r, mut w) = pipe();
        scheduler.spawn(Job::Read { file: Some(r), tag });
        writers.push(std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150 + 50 * u64::from(tag)));
            w.write_all(format!("pipe-{tag}").as_bytes()).unwrap();
        }));
    }
    for tag in 10..14u8 {
        scheduler.spawn(Job::Spin { left: 500, tag });
    }
    let outputs = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&outputs);
    let config = CoSchedule {
        slice: 16,
        ..CoSchedule::default()
    };
    let task = mainloop::co_schedule(thread, scheduler, config, move |o| {
        sink.borrow_mut().push(o)
    })
    .unwrap();
    // Meanwhile GTK keeps handling input: a GLib timeout clicks the button
    // every 10 ms while Pulse waits for the pipes.
    let tap = widget(&h, thread, 0).downcast::<gtk::Button>().unwrap();
    let clicks = Rc::new(Cell::new(0u32));
    let counter = Rc::clone(&clicks);
    let clicker = glib::timeout_add_local(Duration::from_millis(10), move || {
        tap.emit_clicked();
        counter.set(counter.get() + 1);
        glib::ControlFlow::Continue
    });
    let mut routed = 0usize;
    mainloop::pump_until(thread, SETTLE, || {
        routed += h.poll(thread).unwrap().events.len();
        task.is_finished()
    })
    .unwrap();
    clicker.remove();
    for w in writers {
        w.join().unwrap();
    }
    assert!(task.take_error().is_none());
    let stats = task.stats();
    println!("  {stats:?}; {routed} clicks routed while Pulse ran");
    let mut got = outputs.borrow().clone();
    got.sort_by_key(|o| match o {
        PulseOutput::Completed(v) => v[0],
        PulseOutput::Failed(_) => u8::MAX,
    });
    let expected: Vec<PulseOutput> = (0..3u8)
        .map(|t| {
            let mut v = vec![t];
            v.extend_from_slice(format!("pipe-{t}").as_bytes());
            PulseOutput::Completed(v)
        })
        .chain((10..14u8).map(|t| PulseOutput::Completed(vec![t])))
        .collect();
    assert_eq!(got, expected);
    // 4 x 501 spin resumes + 3 x 2 read resumes, in slices of at most 16,
    // yielding to GLib between slices; GTK input was handled meanwhile.
    assert_eq!(stats.resumed, 4 * 501 + 3 * 2);
    assert!(
        stats.max_slice <= 16 && stats.yields >= 2010 / 16,
        "{stats:?}"
    );
    assert!(stats.io_waits > 0, "{stats:?}");
    assert!(
        routed >= 5,
        "GTK input is serviced while Pulse waits ({routed})"
    );
    assert!(task.stop().is_some());
    assert!(matches!(
        mainloop::co_schedule(
            thread,
            Scheduler::new(
                vesper_pulse::select::open::<Job>(8, vesper_pulse::select::Preference::Auto)
                    .unwrap()
                    .0
            ),
            CoSchedule {
                slice: 0,
                ..CoSchedule::default()
            },
            |_| {}
        ),
        Err(GtkError::Unsupported(_))
    ));
    shutdown(h, thread);
}

fn pulse_worker_posts_to_the_ui(thread: &UiThread) {
    let mut h = host(thread);
    render(&mut h, thread, 0, vec![text(0, "waiting")]);
    let (tx, rx) = dispatch::channel::<PulseOutput>(4).unwrap();
    let outputs = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&outputs);
    let inbox = mainloop::spawn_inbox(thread, rx, 8, move |b| sink.borrow_mut().extend(b)).unwrap();
    let (r, mut w) = pipe();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        w.write_all(b"from-worker").unwrap();
    });
    let worker = mainloop::spawn_pulse_worker(
        move || {
            let (driver, _) =
                vesper_pulse::select::open::<Job>(16, vesper_pulse::select::Preference::Auto)
                    .map_err(std::io::Error::other)?;
            let mut s = Scheduler::new(driver);
            s.spawn(Job::Read {
                file: Some(r),
                tag: 1,
            });
            for tag in 20..30 {
                s.spawn(Job::Spin { left: 10, tag });
            }
            Ok(s)
        },
        tx,
    );
    mainloop::pump_until(thread, SETTLE, || outputs.borrow().len() == 11).unwrap();
    writer.join().unwrap();
    assert_eq!(worker.join().unwrap().unwrap(), 11);
    let mut read = b"\x01".to_vec();
    read.extend_from_slice(b"from-worker");
    assert!(outputs.borrow().contains(&PulseOutput::Completed(read)));
    assert!(inbox.close().is_empty());
    // A worker whose inbox is gone stops with BrokenPipe.
    let (tx, rx) = dispatch::channel::<PulseOutput>(1).unwrap();
    drop(rx);
    let worker = mainloop::spawn_pulse_worker(
        || {
            let (driver, _) =
                vesper_pulse::select::open::<Job>(4, vesper_pulse::select::Preference::Auto)
                    .map_err(std::io::Error::other)?;
            let mut s = Scheduler::new(driver);
            s.spawn(Job::Spin { left: 1, tag: 0 });
            Ok(s)
        },
        tx,
    );
    assert_eq!(
        worker.join().unwrap().unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    shutdown(h, thread);
}

fn atspi_tree(thread: &UiThread) -> Option<atspi::AtspiNode> {
    let result = mainloop::run_off_thread(thread, SETTLE, || {
        let client = AtspiClient::connect(Duration::from_secs(5))?;
        client.application(APP_NAME, 2000, 64)
    })
    .unwrap();
    match result {
        Ok(tree) => Some(tree),
        Err(e @ atspi::AtspiError::Unavailable(_)) => {
            assert!(
                std::env::var_os("VESPER_REQUIRE_ATSPI").is_none(),
                "AT-SPI required but unavailable: {e:?}"
            );
            println!("  SKIP: no AT-SPI bus ({e:?}); run under dbus-run-session");
            None
        }
        Err(e) => panic!("AT-SPI: {e:?}"),
    }
}

fn accessibility_tree_through_atspi(thread: &UiThread) {
    let mut h = host(thread);
    h.set_style(thread, Key(0), Style::default().layout(grid(2)))
        .unwrap();
    let tree = |name_field: bool| {
        let field = if name_field {
            input(3, "Name")
        } else {
            Node::new(Key(3), Kind::Input).on(Event::Changed, HandlerKey(3))
        };
        vec![
            container(0, &[1, 2, 3, 4]),
            button(1, "Save").property(Property::Enabled, Value::Bool(false)),
            button(2, "Go"),
            field,
            text(4, "Status"),
        ]
    };
    render(&mut h, thread, 0, tree(true));
    settle(&h, thread);
    let Some(app) = atspi_tree(thread) else {
        shutdown(h, thread);
        return;
    };
    let save = app.find("button", "Save").expect("Save button in AT-SPI");
    // GTK 4.14 reports `sensitive` (not `enabled`) for widget sensitivity.
    assert!(!save.has(State::Sensitive));
    assert!(save.has(State::Showing) && save.has(State::Visible));
    let go = app.find("button", "Go").expect("Go button");
    println!(
        "  Save states {:#x}, Go states {:#x}",
        save.states, go.states
    );
    assert!(go.has(State::Sensitive) && go.has(State::Focusable));
    let name = app
        .find("text box", "Name")
        .expect("entry named by its placeholder");
    assert!(name.has(State::Editable));
    assert!(app.find("label", "Status").is_some());
    assert_eq!(app.all("button").len(), 2);
    assert_eq!(atspi::audit(&app), []);

    // Updates reach AT-SPI: enabling Save flips its states.
    let mut next = tree(true);
    next[1] = button(1, "Save");
    render(&mut h, thread, 0, next);
    settle(&h, thread);
    let app = atspi_tree(thread).unwrap();
    assert!(app.find("button", "Save").unwrap().has(State::Sensitive));

    // An entry without a placeholder has no accessible name; the audit says so.
    render(&mut h, thread, 0, tree(false));
    settle(&h, thread);
    let app = atspi_tree(thread).unwrap();
    let issues = atspi::audit(&app);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].role, "text box");
    shutdown(h, thread);
}

const SHOT_CSS: &str = "
.shot { background-color: #18202a; padding: 6px; }
.swatch { min-width: 36px; min-height: 24px; border-radius: 0; }
.red { background-color: #e04040; }
.green { background-color: #30b050; }
.blue { background-color: #3070e0; }
.gold { background-color: #e0b020; border: 3px solid #ffffff; }
.grey { background-color: #808080; }
.teal { background-color: #20a0a0; }
";

/// Stated tolerance: a pixel differs if any channel is off by more than 8;
/// at most 0.2 % of pixels may differ.
const SHOT_TOLERANCE: Tolerance = Tolerance {
    max_channel_delta: 8,
    max_differing_per_million: 2_000,
};

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name)
}

fn check_golden(name: &str, actual: &RgbaImage) {
    let path = golden(name);
    if std::env::var_os("VESPER_UPDATE_GOLDENS").is_some() {
        actual.save_png(&path).unwrap();
        println!("  wrote {}", path.display());
    }
    let expected = RgbaImage::load_png(&path).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    match image::compare(&expected, actual, SHOT_TOLERANCE) {
        Ok(diff) => println!("  {name}: {diff:?}"),
        Err(m) => {
            let out = std::env::temp_dir().join(format!("actual-{name}"));
            let _ = actual.save_png(&out);
            panic!(
                "{name} differs from its golden: {m:?} (actual saved to {})",
                out.display()
            );
        }
    }
}

fn screenshots_match_goldens(thread: &UiThread) {
    let mut h = host(thread);
    h.backend_mut(thread)
        .unwrap()
        .load_css(thread, SHOT_CSS)
        .unwrap();
    let colours = ["red", "green", "blue", "gold", "grey", "teal"];
    h.set_style(
        thread,
        Key(0),
        Style::default().class("shot").layout(Layout::Grid {
            columns: 3,
            row_spacing: 4,
            column_spacing: 4,
        }),
    )
    .unwrap();
    for (i, c) in colours.iter().enumerate() {
        h.set_style(
            thread,
            Key(i as u64 + 1),
            Style::default().class("swatch").class(*c),
        )
        .unwrap();
    }
    let mut nodes = vec![container(0, &[1, 2, 3, 4, 5, 6])];
    nodes.extend((1..=6).map(|k| container(k, &[])));
    render(&mut h, thread, 0, nodes.clone());
    settle(&h, thread);
    let shot = h.backend(thread).unwrap().screenshot(thread).unwrap();
    let root = widget(&h, thread, 0);
    assert_eq!(
        (shot.width() as i32, shot.height() as i32),
        (root.width(), root.height())
    );
    // Independent of the golden: GTK's own allocations say where each
    // swatch is, and the pixels there have the stylesheet's colours.
    let near = |p: [u8; 4], q: [u8; 3]| {
        p[..3].iter().zip(q).all(|(a, b)| a.abs_diff(b) <= 8) && p[3] == 255
    };
    // GTK widget coordinates start at the content box; the screenshot
    // starts at the root's border box (its own bounds in its own space).
    let own = root.compute_bounds(&root).unwrap();
    let at = |shot: &RgbaImage, w: gtk::Widget, dx: f32, dy: f32| {
        let b = w.compute_bounds(&root).unwrap();
        let (x, y) = (
            b.x() - own.x() + dx.min(b.width() / 2.0),
            b.y() - own.y() + dy.min(b.height() / 2.0),
        );
        shot.pixel(x as u32, y as u32).unwrap()
    };
    let centre = f32::MAX;
    assert!(near(
        at(&shot, widget(&h, thread, 1), centre, centre),
        [0xe0, 0x40, 0x40]
    ));
    assert!(near(
        at(&shot, widget(&h, thread, 2), centre, centre),
        [0x30, 0xb0, 0x50]
    ));
    assert!(near(
        at(&shot, widget(&h, thread, 6), centre, centre),
        [0x20, 0xa0, 0xa0]
    ));
    assert!(
        near(
            at(&shot, widget(&h, thread, 4), 1.0, 1.0),
            [0xff, 0xff, 0xff]
        ),
        "gold border"
    );
    assert!(near(
        at(&shot, widget(&h, thread, 4), centre, centre),
        [0xe0, 0xb0, 0x20]
    ));
    assert!(
        near(shot.pixel(1, 1).unwrap(), [0x18, 0x20, 0x2a]),
        "root padding"
    );
    // Grid geometry: red and green share a row; gold starts the second row.
    let (r, g, y) = (
        origin(&widget(&h, thread, 1), &root),
        origin(&widget(&h, thread, 2), &root),
        origin(&widget(&h, thread, 4), &root),
    );
    assert!(r.1 == g.1 && g.0 > r.0 && y.0 == r.0 && y.1 > r.1);
    check_golden("grid-swatches.png", &shot);

    // Reorder (keys keep their classes) and switch to a horizontal stack.
    h.set_style(
        thread,
        Key(0),
        Style::default()
            .class("shot")
            .layout(Layout::HStack { spacing: 2 }),
    )
    .unwrap();
    nodes[0] = container(0, &[6, 5, 4, 3, 2, 1]);
    render(&mut h, thread, 0, nodes);
    settle(&h, thread);
    let shot2 = h.backend(thread).unwrap().screenshot(thread).unwrap();
    assert!(
        shot2.width() > shot2.height() * 3,
        "one row of six swatches"
    );
    assert!(near(
        at(&shot2, widget(&h, thread, 6), centre, centre),
        [0x20, 0xa0, 0xa0]
    ));
    let (teal, red) = (
        origin(&widget(&h, thread, 6), &root),
        origin(&widget(&h, thread, 1), &root),
    );
    assert!(teal.1 == red.1 && teal.0 < red.0, "reordered left to right");
    check_golden("hstack-swatches.png", &shot2);

    // A real visual change exceeds the tolerance.
    h.backend_mut(thread)
        .unwrap()
        .load_css(thread, ".teal { background-color: #a02020; }")
        .unwrap();
    settle(&h, thread);
    let changed = h.backend(thread).unwrap().screenshot(thread).unwrap();
    let golden2 = RgbaImage::load_png(&golden("hstack-swatches.png")).unwrap();
    assert!(matches!(
        image::compare(&golden2, &changed, SHOT_TOLERANCE),
        Err(image::Mismatch::Pixels(d)) if d.differing >= 36 * 24
    ));
    drop(root);
    shutdown(h, thread);
}

type Case = (&'static str, fn(&UiThread));

fn main() {
    glib::set_prgname(Some(APP_NAME));
    let thread = UiThread::claim_current();
    let cases: &[Case] = &[
        ("layouts_follow_keys", layouts_follow_keys),
        ("css_classes_and_stylesheets", css_classes_and_stylesheets),
        ("virtual_list_recycles_rows", virtual_list_recycles_rows),
        ("close_button_requests_close", close_button_requests_close),
        (
            "dispatcher_wakes_the_main_loop",
            dispatcher_wakes_the_main_loop,
        ),
        ("pulse_co_schedules_with_gtk", pulse_co_schedules_with_gtk),
        ("pulse_worker_posts_to_the_ui", pulse_worker_posts_to_the_ui),
        (
            "accessibility_tree_through_atspi",
            accessibility_tree_through_atspi,
        ),
        ("screenshots_match_goldens", screenshots_match_goldens),
    ];
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut ran = 0;
    for (name, case) in cases {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        println!("test {name} ...");
        case(&thread);
        println!("test {name} ... ok");
        ran += 1;
    }
    println!("\ntest result: ok. {ran} passed; 0 failed");
}
