# Legacy Treer Mail service

The Rust `treer-mail` service was replaced by the CLI-only
[Mail plugin](../../plugins/mail/README.md). Canonical Message data, context
edges, deliveries, and acknowledgements now live in Treer Core; this directory
is retained only so links from older deployment notes reach the migration
procedure.

Do not start the old service after cutover. Back up its SQLite or PostgreSQL
database, stop writes, and follow the plugin's documented `migrate.py` workflow.
The migration never deletes or modifies the source database. Legacy browser
sessions require one new login because their broad App token cannot be safely
converted into a plugin-bound human capability.
