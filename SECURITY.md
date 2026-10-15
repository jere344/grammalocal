# Security Policy

## Supported versions

Only the latest release receives security fixes.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub's private vulnerability reporting: go to the
[Security tab](https://github.com/jere344/grammalocal/security) of the
repository and choose **Report a vulnerability**. This keeps the report visible
only to the maintainers until a fix is ready.

When reporting, please include:

- A description of the problem and its impact.
- Steps to reproduce, or a proof of concept.
- The version or commit you tested, and your operating system.

You can expect an acknowledgement within a week. Confirmed issues will be fixed
in a new release, and the advisory will credit the reporter unless they prefer
to stay anonymous.

## Scope

Grammalocal is designed to run on your own machine. The daemon listens on
`127.0.0.1` only and requires a bearer token for write endpoints. Reports about
the following are especially welcome:

- Ways for a remote host or another local user to read text sent for checking,
  or to use the API without the token.
- Problems with the installer, such as checksum bypass or unsafe file handling.
- Problems in the systemd unit or the desktop entry that allow privilege
  escalation.
