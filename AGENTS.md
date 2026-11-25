# Repository automation

Never add new Python, Bash, or other shell scripts, including scripts without a file extension. Use native Aspect tasks written in AXL for repository automation. Use PowerShell (`.ps1`) for Windows entry points that cannot run Aspect.

This rule also applies to scripts embedded in GitHub Actions workflows and other configuration files. Keep logic out of inline `run` blocks, heredocs, and interpreter command strings such as `python -c` and `bash -c`. Configure automation to invoke the Aspect task or Windows PowerShell entry point directly.

When migrating an existing script, update its callers and tests to use the replacement and remove the original script.
