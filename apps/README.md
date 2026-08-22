# Legacy workspace apps

Channel applications now use the CLI-only script plugin contract under
[`plugins`](../plugins/README.md). This directory retains migration pointers for
operators upgrading an older deployment; it does not own current runtime code.

- [`mail`](mail/README.md) points from the removed Rust Mail service to the
  current plugin and legacy database migration procedure.
