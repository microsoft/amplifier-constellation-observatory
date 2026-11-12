//! Opt-in registration/query/removal smoke for disposable platform CI runners.
//! It never launches the registered test executable or pairs a real computer.
use constellation_machine_helper::platform;
use std::{fs, process::Command};
#[test]
#[ignore = "writes a unique current-user startup entry; run only in disposable CI"]
fn native_startup_registration_query_and_removal() {
    assert_eq!(
        std::env::var("CONSTELLATION_STARTUP_SMOKE").as_deref(),
        Ok("1"),
        "Explicit disposable-runner opt-in required"
    );
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("helper with spaces");
    fs::create_dir(&home).unwrap();
    let id = uuid::Uuid::new_v4();
    let label = format!("constellation-machine-{id}");
    platform::install_startup(&home, &id).unwrap();
    #[cfg(target_os = "linux")]
    {
        assert!(
            Command::new("systemctl")
                .args(["--user", "is-enabled", &format!("{label}.service")])
                .status()
                .unwrap()
                .success()
        );
        let path = platform::user_home()
            .unwrap()
            .join(".config/systemd/user")
            .join(format!("{label}.service"));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            platform::systemd_unit(&platform::installed_binary(&home), &home).unwrap()
        );
    }
    #[cfg(target_os = "macos")]
    {
        let path = platform::user_home()
            .unwrap()
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        assert!(
            Command::new("plutil")
                .arg("-lint")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            platform::launch_agent(&label, &platform::installed_binary(&home), &home).unwrap()
        );
    }
    #[cfg(windows)]
    {
        assert!(
            Command::new("schtasks.exe")
                .args(["/Query", "/TN", &label, "/XML"])
                .status()
                .unwrap()
                .success()
        );
    }
    // A second registration is exact/idempotent and never replaces another task.
    platform::install_startup(&home, &id).unwrap();
    platform::remove_startup(&home, &id).unwrap();
    #[cfg(target_os = "linux")]
    assert!(
        !platform::user_home()
            .unwrap()
            .join(".config/systemd/user")
            .join(format!("{label}.service"))
            .exists()
    );
    #[cfg(target_os = "macos")]
    assert!(
        !platform::user_home()
            .unwrap()
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
            .exists()
    );
    #[cfg(windows)]
    assert!(
        !Command::new("schtasks.exe")
            .args(["/Query", "/TN", &label])
            .status()
            .unwrap()
            .success()
    );
}
