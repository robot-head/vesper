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
// The doctest source guard words (SH5 lane 14 part 5), shared with the
// root package's tests/self_host_doctest_guard.rs.
#[path = "../../../../tests/common/doctest_guard_words.rs"]
mod doctest_guard_words;
// The doctest module words (SH5 lane 14 part 6), shared with the root
// package's tests/self_host_doctest_generate.rs.
#[path = "../../../../tests/common/doctest_generate_words.rs"]
mod doctest_generate_words;
// The doctest wrapper-name and verdict words (SH5 lane 14 part 7), shared
// with the root package's tests/self_host_doctest_verdict.rs.
#[path = "../../../../tests/common/doctest_verdict_words.rs"]
mod doctest_verdict_words;
// The test report words (SH5 lane 14 part 8), shared with the root
// package's tests/self_host_test_report.rs.
#[path = "../../../../tests/common/test_report_words.rs"]
mod test_report_words;

use super::{Registry, Swap, must, on, open, pass};

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

/// `vesper::doctest_guard::DocTestGuardTable` from `modules/test_runner`:
/// the doctest source profile of `@vesper/test-runner`, which parses the
/// source again with `@vesper/ast`.
const VESPER_DOCTEST_GUARD: vesper::doctest_guard::DocTestGuardTable =
    vesper::doctest_guard::DocTestGuardTable {
        guard: |source, _, limits| {
            let input = doctest_guard_words::guard_input(source, limits);
            doctest_guard_words::decode_guard(&test_runner_binding::doctest_guard_words(&input))
        },
    };

/// `vesper::doctest_generate::DocTestGenerateTable` from
/// `modules/test_runner`: the generated module of a doctest example from
/// `@vesper/test-runner`.
const VESPER_DOCTEST_GENERATE: vesper::doctest_generate::DocTestGenerateTable =
    vesper::doctest_generate::DocTestGenerateTable {
        generate: |input| {
            let words = doctest_generate_words::generate_input(input);
            doctest_generate_words::decode_generate(&test_runner_binding::doctest_generate_words(
                &words,
            ))
        },
    };

/// `vesper::doctest_verdict::DocTestVerdictTable` from `modules/test_runner`:
/// the doctest wrapper names and check-time verdicts of
/// `@vesper/test-runner`.
const VESPER_DOCTEST_VERDICT: vesper::doctest_verdict::DocTestVerdictTable =
    vesper::doctest_verdict::DocTestVerdictTable {
        wrappers: |functions, examples| {
            let input = doctest_verdict_words::wrappers_input(functions, examples);
            doctest_verdict_words::decode_wrappers(&test_runner_binding::doctest_wrappers_words(
                &input,
            ))
        },
        verdict: |input| {
            let words = doctest_verdict_words::verdict_input(input);
            doctest_verdict_words::decode_verdict(&test_runner_binding::doctest_verdict_words(
                &words,
            ))
        },
    };

/// `vesper::test_report::TestReportTable` from `modules/test_runner`: the
/// text reports of `vesper test` and `vesper test --doc` from
/// `@vesper/test-runner`.
const VESPER_TEST_REPORT: vesper::test_report::TestReportTable =
    vesper::test_report::TestReportTable {
        doc_text: |file, base, examples| {
            let input = test_report_words::doc_input(file, base, examples);
            test_report_words::decode_text(&test_runner_binding::report_doc_text_words(&input))
        },
        unit_text: |files| {
            let input = test_report_words::unit_input(files);
            test_report_words::decode_text(&test_runner_binding::report_unit_text_words(&input))
        },
        baseline_text: |files| {
            let input = test_report_words::unit_input(files);
            test_report_words::decode_text(&test_runner_binding::report_baseline_text_words(&input))
        },
        mutants_text: |mutants, counts| {
            let input = test_report_words::mutants_input(mutants, counts);
            test_report_words::decode_text(&test_runner_binding::report_mutants_text_words(&input))
        },
    };

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[
    open("test_discovery", "no flip-gate configuration"),
    pass("doctest_extract", "doctest-extract", "#2207"),
    pass("test_mutants", "test-mutants", "#2207"),
    pass("doctest_guard", "doctest-guard", "#2207"),
    pass("doctest_generate", "doctest-generate", "#2207"),
    pass("doctest_verdict", "doctest-verdict", "#2207"),
    open(
        "test_report",
        "0 diffs, 1.8x end to end (word boundary), 1.1x rendering alone; no flip-gate configuration",
    ),
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
    if on("doctest_guard") {
        must(vesper::doctest_guard::install_doctest_guard(
            VESPER_DOCTEST_GUARD,
        ));
    }
    if on("doctest_generate") {
        must(vesper::doctest_generate::install_doctest_generate(
            VESPER_DOCTEST_GENERATE,
        ));
    }
    if on("doctest_verdict") {
        must(vesper::doctest_verdict::install_doctest_verdict(
            VESPER_DOCTEST_VERDICT,
        ));
    }
    if on("test_report") {
        must(vesper::test_report::install_test_report(VESPER_TEST_REPORT));
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: test-runner answered {} calls from Vesper",
        TEST_RUNNER_CALLS.load(Relaxed),
    );
}
