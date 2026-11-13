# Security Policy

## Supported versions

Only the latest commit on `main` is supported while the project is pre-1.0.

## Reporting a vulnerability

Please do not open a public issue. Report privately through [GitHub Security Advisories](https://github.com/okita-io/rsyncstat/security/advisories/new). Include steps to reproduce and the affected platform. You can expect an acknowledgement within a week.

Areas of particular interest: anything that could delete, overwrite or leak files outside the chosen roots (path traversal, symlink handling, sync execution).
