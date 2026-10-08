Vendored from librespot-core 0.8.0 (https://github.com/librespot-org/librespot, MIT license).

onify changes: src/session.rs no longer exits the whole process when the account isn't
Premium (check_catalogue); onify checks the account's "type" attribute itself and shows
its login page with a message instead.
src/authentication.rs: one #[expect(deprecated)] is #[allow(deprecated)], so the vendored copy
builds without a warning on newer Rust.
