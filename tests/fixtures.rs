//! End-to-end coverage of every supported language, driving the real binary
//! over the committed fixture projects (fixtures/<service> per language,
//! declared in repomap.toml). Each test asserts the pointer a user would see.

use std::process::{Command, Output};

fn repomap(db: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_repomap"))
        .arg("--root")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn stdout_of(db: &std::path::Path, args: &[&str]) -> String {
    let out = repomap(db, args);
    assert!(
        out.status.success(),
        "repomap {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn rust_fixture_resolves_module_qualified_callers() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    let def = stdout_of(&db, &["def", "reserve", "--service", "inventory"]);
    assert!(def.contains("fixtures/inventory/src/stock.rs:L"), "{def}");

    // main() calls `stock::reserve(3)` — a module-qualified call.
    let callers = stdout_of(&db, &["callers", "reserve", "--service", "inventory"]);
    assert!(
        callers.contains("fixtures/inventory/src/main.rs:L"),
        "module-qualified caller missing:\n{callers}"
    );
}

#[test]
fn rust_fixture_rank_hides_cfg_test_helpers_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    let default = stdout_of(&db, &["rank", "--service", "inventory", "-k", "20"]);
    assert!(
        !default.contains("stocked") && !default.contains("reserve_never_underflows"),
        "test code leaked into default rank:\n{default}"
    );
    let with_tests = stdout_of(
        &db,
        &[
            "rank",
            "--service",
            "inventory",
            "-k",
            "20",
            "--include-tests",
        ],
    );
    assert!(
        with_tests.contains("stocked"),
        "--include-tests must show test helpers:\n{with_tests}"
    );
}

#[test]
fn python_fixture_resolves_imported_module_calls() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    // orders.py does `import pricing` then calls `pricing.unit_price(...)`.
    let callers = stdout_of(&db, &["callers", "unit_price", "--service", "catalog"]);
    assert!(
        callers.contains("fixtures/catalog/orders.py:L"),
        "imported-module caller missing:\n{callers}"
    );

    // tests/test_orders.py is test code by path convention.
    let outline = stdout_of(&db, &["--format", "jsonl", "find", "test_order_total"]);
    assert!(outline.contains("test_orders.py"), "{outline}");
}

#[test]
fn typescript_fixture_links_named_imports_and_classes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    let def = stdout_of(&db, &["def", "Cart", "--service", "storefront"]);
    assert!(def.contains("fixtures/storefront/src/cart.ts:L"), "{def}");

    let callers = stdout_of(&db, &["callers", "unitPrice", "--service", "storefront"]);
    assert!(
        callers.contains("fixtures/storefront/src/cart.ts:L"),
        "{callers}"
    );
}

#[test]
fn ruby_fixture_links_class_owned_calls() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    let def = stdout_of(&db, &["def", "for_zone", "--service", "shipping"]);
    assert!(def.contains("fixtures/shipping/lib/rates.rb:L"), "{def}");

    // Quote#total calls `Rates.for_zone(zone)`.
    let callers = stdout_of(&db, &["callers", "for_zone", "--service", "shipping"]);
    assert!(
        callers.contains("fixtures/shipping/lib/quote.rb:L"),
        "{callers}"
    );
}

#[test]
fn scala_fixture_still_links_qualified_object_calls() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("idx.db");

    let callers = stdout_of(&db, &["callers", "withTax", "--service", "billing"]);
    assert!(
        callers.contains("fixtures/billing/src/main/scala/billing/Invoice.scala:L"),
        "{callers}"
    );
}
