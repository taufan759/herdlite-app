// Server latar belakang herdlite. Subsystem "windows" = tanpa jendela konsol.
// `herdlited.exe --open` (dipakai shortcut Start Menu): nyalakan server kalau
// belum jalan, lalu buka dashboard di browser, mirip membuka aplikasi Herd.
#![windows_subsystem = "windows"]

use herdlite::{config, server, util, win};
use std::time::{Duration, Instant};

fn main() {
    let home = config::home();
    let log = home.join("logs").join("herdlite.log");
    let open = std::env::args().any(|a| a == "--open");
    let pidp = server::pid_path(&home);
    if let Some(pid) = std::fs::read_to_string(&pidp).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
        if pid != std::process::id() && win::pid_alive(pid) {
            if open {
                if let Ok(cfg) = config::Config::load(&home) {
                    server::open_url(&server::dashboard_url(&cfg));
                }
            } else {
                util::append_log(&log, &format!("herdlited sudah berjalan (pid {pid}), instance baru keluar"));
            }
            return;
        }
    }
    if open {
        let home2 = home.clone();
        std::thread::spawn(move || {
            let Ok(cfg) = config::Config::load(&home2) else { return };
            let addr = format!("{}:{}", if cfg.bind == "0.0.0.0" { "127.0.0.1" } else { &cfg.bind }, cfg.http_port);
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(10) {
                if std::net::TcpStream::connect(&addr).is_ok() {
                    server::open_url(&server::dashboard_url(&cfg));
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
    win::kill_children_on_exit();
    if let Err(e) = server::run(home.clone()) {
        util::append_log(&log, &format!("ERROR: {e}"));
        let _ = std::fs::remove_file(&pidp);
        std::process::exit(1);
    }
}
