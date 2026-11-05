// Server latar belakang herdlite. Subsystem "windows" = tanpa jendela konsol.
#![windows_subsystem = "windows"]

use herdlite::{config, server, util, win};

fn main() {
    let home = config::home();
    let log = home.join("logs").join("herdlite.log");
    let pidp = server::pid_path(&home);
    if let Some(pid) = std::fs::read_to_string(&pidp).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
        if pid != std::process::id() && win::pid_alive(pid) {
            util::append_log(&log, &format!("herdlited sudah berjalan (pid {pid}), instance baru keluar"));
            return;
        }
    }
    win::kill_children_on_exit();
    if let Err(e) = server::run(home.clone()) {
        util::append_log(&log, &format!("ERROR: {e}"));
        let _ = std::fs::remove_file(&pidp);
        std::process::exit(1);
    }
}
