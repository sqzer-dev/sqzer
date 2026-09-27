# Security policy

`sqzer` parses untrusted images, so a malformed file that crashes it, hangs it or makes it allocate without bound is a security bug.

## Supported versions

Only the latest release gets fixes. Before `1.0` there are no backports.

## Reporting a vulnerability

Report it privately through [GitHub's private vulnerability reporting](https://github.com/sqzer-dev/sqzer/security/advisories/new). Do not open a public issue. Include:

```text
sqzer --version and sqzer --list-codecs
the command line
the input file, or how to produce it
what happened: crash output, memory use, time taken
```

`sqzer` is maintained by one person. Expect an acknowledgement within a week, and a fix or a plan once the cause is known. A fix ships in a release with an advisory that credits you, unless you ask not to be named.

## Scope

In scope:

- a panic, crash, hang or unbounded allocation on a crafted input
- an input that gets past `DecodeOpts::max_pixels` or the `-j` memory budget
- memory safety issues in the three crates that contain `unsafe`: `heif-dl`, `heif-imageio` and `heif-wic`
- output written somewhere the command line did not ask for

> **Note**: Most decoding happens in upstream crates and C libraries (`zune-jpeg`, `png`, `jxl-oxide`, `libwebp`, `libheif` and others). If the bug is in one of them, report it here anyway: we coordinate with upstream and make sure `sqzer` picks up the fix or works around it.

Out of scope: a large but bounded amount of memory or time on a large input that stays within the limits, and anything that needs an attacker who can already change the command line or the environment.
