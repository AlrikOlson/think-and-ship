//! Regression test for Phase 15e1: think and ship persistence must not
//! share an on-disk path when configured against the same data dir, or
//! they will silently clobber each other's `<project>.json` files.

use std::path::PathBuf;

use tempfile::TempDir;

use think_and_ship::ship::persistence::Persistence as ShipPersistence;
use think_and_ship::ship::persistence::PersistenceConfig as ShipPersistenceConfig;
use think_and_ship::think::config::PersistenceConfig as ThinkPersistenceConfig;
use think_and_ship::think::persistence::Persistence as ThinkPersistence;

#[test]
fn think_and_ship_persistence_live_in_disjoint_subdirs() {
    let tmp = TempDir::new().unwrap();
    let data_dir: PathBuf = tmp.path().to_path_buf();

    let think = ThinkPersistence::new(&ThinkPersistenceConfig {
        enabled: true,
        data_dir: data_dir.clone(),
    });
    let ship = ShipPersistence::new(&ShipPersistenceConfig {
        enabled: true,
        data_dir: data_dir.clone(),
    });

    // think exposes its sessions_dir; ship does not, so we reconstruct
    // the expected path and assert both physical directories were
    // created (the constructors mkdir on demand when enabled=true).
    let think_dir = think.sessions_dir();
    let ship_dir = data_dir.join("ship").join("sessions");

    assert!(
        think_dir.ends_with("think/sessions"),
        "think sessions_dir should end with `think/sessions`, got {}",
        think_dir.display()
    );
    assert!(
        ship_dir.exists(),
        "ship sessions dir should be created at {}",
        ship_dir.display()
    );
    assert_ne!(
        think_dir,
        ship_dir.as_path(),
        "think and ship persistence must not share a directory; \
         think_dir={} ship_dir={}",
        think_dir.display(),
        ship_dir.display()
    );

    // Keep ship borrowed so the test exercises both handles, not just
    // think. The constructor side effects (mkdir) are the load-bearing
    // observation here.
    let _ = &ship;
}
