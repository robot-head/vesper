// Test discovery and doctest extraction of `vesper test` (SH5 lane 14,
// #1337), from `modules/test_runner`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/test_runner.rs"]
mod test_runner_binding;

// The test-discovery swap's input encoders and result decoders (#1337),
// shared with the root package's tests/self_host_test_runner.rs.
#[path = "../../../../tests/common/test_runner_words.rs"]
mod test_runner_words;

// The doctest extraction words (SH5 lane 14 part 2), shared with the root
// package's tests/self_host_doctest_extract.rs.
#[path = "../../../../tests/common/doctest_extract_words.rs"]
mod doctest_extract_words;

// The mutation-site words (SH5 lane 14 part 3), shared with the root
// package's tests/self_host_test_mutants.rs.
#[path = "../../../../tests/common/test_mutants_words.rs"]
mod test_mutants_words;

use super::{Registry, Swap, must, on, open};

/// Sources whose tests the Vesper test-runner module discovered.
static TEST_RUNNER_CALLS: AtomicU64 = AtomicU64::new(0);

/// `vesper::test_discovery::TestDiscoveryTable` from `modules/test_runner`:
/// `@case` rows and test discovery of `@vesper/test-runner`.
const VESPER_TEST_DISCOVERY: vesper::test_discovery::TestDiscoveryTable =
    vesper::test_discovery::TestDiscoveryTable {
        case_rows: |source, module| {
            let input = test_runner_words::case_rows_input(source, module);
            test_runner_words::decode_case_rows(
                source,
                &test_runner_binding::case_rows_words(&input),
            )
        },
        discover: |source, items, cases| {
            TEST_RUNNER_CALLS.fetch_add(1, Relaxed);
            let input = test_runner_words::discover_input(source, items, cases);
            test_runner_words::decode_discover(&test_runner_binding::discover_words(&input))
        },
    };

/// `vesper::doctest_extract::DocTestExtractTable` from `modules/test_runner`:
/// the doctest fence extraction and token preflight of `@vesper/test-runner`.
const VESPER_DOCTEST_EXTRACT: vesper::doctest_extract::DocTestExtractTable =
    vesper::doctest_extract::DocTestExtractTable {
        preflight: |source, limits| {
            let input = doctest_extract_words::preflight_input(source, limits);
            doctest_extract_words::decode_preflight(&test_runner_binding::doctest_preflight_words(
                &input,
            ))
        },
        extract: |source, limits| {
            let input = doctest_extract_words::extract_input(source, limits);
            doctest_extract_words::decode_extract(
                source,
                &test_runner_binding::doctest_extract_words(&input),
            )
        },
    };

/// `vesper::test_mutants::TestMutantsTable` from `modules/test_runner`: the
/// mutation sites of `@vesper/test-runner` (over `@vesper/ast`).
const VESPER_TEST_MUTANTS: vesper::test_mutants::TestMutantsTable =
    vesper::test_mutants::TestMutantsTable {
        collect: |source, test_names| {
            let input = test_mutants_words::collect_input(source, test_names);
            test_mutants_words::decode_collect(&test_runner_binding::mutants_words(&input))
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("test_discovery", "no flip-gate configuration"),
    open(
        "doctest_extract",
        "0 diffs, 1.12x; no flip-gate configuration",
    ),
    open("test_mutants", "0 diffs, 0.48x; no flip-gate configuration"),
];

pub(super) fn register(_r: &mut Registry) {
    if on("test_discovery") {
        must(vesper::test_discovery::install_test_discovery(
            VESPER_TEST_DISCOVERY,
        ));
    }
    if on("doctest_extract") {
        must(vesper::doctest_extract::install_doctest_extract(
            VESPER_DOCTEST_EXTRACT,
        ));
    }
    if on("test_mutants") {
        must(vesper::test_mutants::install_test_mutants(
            VESPER_TEST_MUTANTS,
        ));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: test-runner answered {} calls from Vesper",
        TEST_RUNNER_CALLS.load(Relaxed),
    );
}
