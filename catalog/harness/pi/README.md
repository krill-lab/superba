# pi JSON Schemas (vendored)

`models.schema.json` and `settings.schema.json` are copied unchanged from the npm
package `@earendil-works/pi-coding-agent@1.3.0` (`schemas/`), MIT licensed
(https://github.com/earendil-works/pi). Superba checks the `models.json` it is
about to write against `models.schema.json` (`superba-config::harness::pi`):
pi rejects the whole file — every custom provider — when one value does not
match. Refresh both files from a newer package when pi's schema changes.
