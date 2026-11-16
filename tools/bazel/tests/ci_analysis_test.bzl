"""Regressions for test environment scope and incremental compilation inputs."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _environment_test_impl(ctx):
    env = analysistest.begin(ctx)
    target = analysistest.target_under_test(env)
    asserts.equals(env, ctx.attr.expected, target[RunEnvironmentInfo].inherited_environment)
    return analysistest.end(env)

environment_test = analysistest.make(
    _environment_test_impl,
    attrs = {"expected": attr.string_list()},
)

def _compile_inputs_test_impl(ctx):
    env = analysistest.begin(ctx)
    inputs = {}
    for action in analysistest.target_actions(env):
        for file in action.inputs.to_list():
            inputs[file.short_path] = True
    for path in ctx.attr.required:
        asserts.true(env, path in inputs, "missing embedded input: " + path)
    for path in ctx.attr.forbidden:
        asserts.false(env, path in inputs, "unrelated compilation input: " + path)
    return analysistest.end(env)

compile_inputs_test = analysistest.make(
    _compile_inputs_test_impl,
    attrs = {
        "required": attr.string_list(),
        "forbidden": attr.string_list(),
    },
)

def _runfiles_test_impl(ctx):
    env = analysistest.begin(ctx)
    target = analysistest.target_under_test(env)
    paths = {file.short_path: True for file in target[DefaultInfo].default_runfiles.files.to_list()}
    for path in ctx.attr.required:
        asserts.true(env, path in paths, "missing runtime input: " + path)
    for path in ctx.attr.forbidden:
        asserts.false(env, path in paths, "unrelated runtime input: " + path)
    return analysistest.end(env)

runfiles_test = analysistest.make(
    _runfiles_test_impl,
    attrs = {
        "required": attr.string_list(),
        "forbidden": attr.string_list(),
    },
)
