# Repository automation

Never add new Python, Bash, or other shell scripts, including scripts without a file extension. Use native Aspect tasks written in AXL for repository automation. Use PowerShell (`.ps1`) for Windows entry points that cannot run Aspect.

When migrating an existing script, update its callers and tests to use the replacement and remove the original script.
