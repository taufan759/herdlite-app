//! hl — CLI herdlite. Perintahnya sengaja mirip `herd` / `valet`.

use herdlite::config::{self, norm, parse_ver, Config, Kind};
use herdlite::util::clean_abs;
use herdlite::{herdimport, hosts, node, phpinstall, server, tls, util, win};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HELP: &str = r#"herdlite — pengganti Laravel Herd yang ringan

Paling mudah:
  hl setup                      siapkan semuanya sekaligus (PHP, Composer, Node, PATH, port 80/443,
                                HTTPS, hosts lewat UAC, autostart, shortcut), lalu tampilkan hl doctor
  hl uninstall [--yes]          bersihkan autostart, shortcut, PATH, CA, dan hosts (folder proyek aman)

Server:
  hl start | stop | restart     nyalakan / matikan server latar belakang
  hl status                     status server dan php-cgi yang sedang jalan
  hl log                        tampilkan log terakhir

Situs (sama seperti Herd/Valet):
  hl park [folder]              setiap subfolder jadi <subfolder>.test (default: folder sekarang)
  hl unpark [folder]
  hl parked                     daftar folder park
  hl link [nama]                tautkan folder sekarang sebagai <nama>.test
  hl unlink [nama]
  hl links                      daftar link
  hl sites                      semua situs + versi PHP + URL
  hl open [nama]                buka situs di browser (tanpa nama: situs folder sekarang / dashboard)

PHP:
  hl use <versi>                set PHP global, mis. hl use 8.4
  hl isolate <versi> [--site=nama]   PHP khusus untuk situs (default: situs folder sekarang)
  hl unisolate [--site=nama]
  hl isolated                   daftar situs yang di-isolate
  hl php [argumen...]           jalankan php sesuai versi situs di folder sekarang
  hl which-php                  versi & path php untuk folder sekarang
  hl php:list                   versi PHP terpasang
  hl php:available              versi PHP yang bisa dipasang dari windows.php.net
  hl php:install <versi> [--force]   unduh & pasang PHP resmi (NTS x64)
  hl php:add <versi> <folder>   daftarkan PHP yang sudah ada di folder lain
  hl composer:install           unduh composer.phar + shim composer.cmd
  hl laravel:install            pasang Laravel installer (perintah `laravel new`)

Node.js (pengganti nvm bawaan Herd):
  hl node:install <versi|lts>   unduh Node resmi dari nodejs.org, mis. hl node:install 26
  hl node:use <versi>           ganti versi Node aktif
  hl node:list                  versi Node terpasang

HTTPS:
  hl secure [nama] | hl secure --all     paksa https untuk situs / semua situs
  hl unsecure [nama] | hl unsecure --all
  hl trust                      percayakan CA herdlite (CurrentUser\Root, ada dialog konfirmasi Windows)
  hl untrust
  hl ca:renew                   buat ulang CA HTTPS dengan batasan terbaru (2 dialog Windows)

Lain-lain:
  hl hosts                      cek nama .test yang belum ada di hosts file
  hl hosts sync                 tulis blok herdlite ke hosts (muncul jendela UAC, klik Yes)
  hl hosts clear                hapus blok herdlite dari hosts
  hl import-herd [--use-herd-php]    salin pengaturan park/link/isolate dari Herd (Herd tidak diubah)
  hl config [kunci [nilai]]     lihat / ubah config.json (mis. hl config http_port 8080)
  hl doctor                     cek kesiapan pindah dari Herd (php/composer/node/hosts/port)
  hl path:add                   tambahkan <home>\bin dan Node aktif ke PATH user
  hl autostart on|off           jalankan herdlite otomatis saat login
  hl shortcut [--desktop]       shortcut "herdlite" di Start Menu: klik = nyala + buka dashboard
  hl home                       tampilkan folder data herdlite
  hl lock                       kunci izin folder herdlite (otomatis dijalankan oleh hl setup)
"#;

fn main() {
    // Output dipotong (mis. `hl sites | head`) bukan error: keluar diam-diam, jangan panic.
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("failed printing to std") {
            std::process::exit(0);
        }
        eprintln!("✗ {msg}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("✗ {e}");
            std::process::exit(1);
        }
    }
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn opt(args: &[String], name: &str) -> Option<String> {
    let p = format!("{name}=");
    args.iter().find_map(|a| a.strip_prefix(&p).map(|s| s.to_string()))
}

fn positional(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| !a.starts_with("--")).cloned().collect()
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn run(args: Vec<String>) -> Result<i32, String> {
    let home = config::home();
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    let pos = positional(&rest);

    // `hl php ...` harus meneruskan argumen apa adanya.
    if cmd == "php" {
        return php_passthru(&home, &rest);
    }

    let mut cfg = Config::load(&home)?;
    match cmd.as_str() {
        "help" | "-h" | "--help" => print!("{HELP}"),
        "version" | "--version" | "-V" => println!("herdlite {}", herdlite::VERSION),
        "home" => println!("{}", home.display()),

        "start" => start(&home, &cfg)?,
        "stop" => stop(&home, true),
        "restart" => {
            stop(&home, false);
            start(&home, &cfg)?;
        }
        "status" => status(&home, &cfg),
        "log" | "logs" => {
            for f in ["herdlite.log", "php.log"] {
                let p = home.join("logs").join(f);
                println!("── {} ──", p.display());
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                let lines: Vec<&str> = t.lines().collect();
                for l in &lines[lines.len().saturating_sub(30)..] {
                    println!("{l}");
                }
            }
        }

        "park" => {
            let dir = pos.first().map(PathBuf::from).unwrap_or_else(cwd);
            if !dir.is_dir() {
                return Err(format!("folder tidak ada: {}", dir.display()));
            }
            let d = clean_abs(&dir);
            if !cfg.paths.iter().any(|p| util::path_key(Path::new(p)) == util::path_key(Path::new(&d))) {
                cfg.paths.push(d.clone());
                cfg.save(&home)?;
            }
            println!("✓ {d} di-park. Setiap subfolder bisa dibuka di http://<subfolder>.{}", cfg.tld);
            hosts_hint(&cfg);
        }
        "unpark" | "forget" => {
            let dir = pos.first().map(PathBuf::from).unwrap_or_else(cwd);
            let k = util::path_key(Path::new(&clean_abs(&dir)));
            let before = cfg.paths.len();
            cfg.paths.retain(|p| util::path_key(Path::new(p)) != k);
            if cfg.paths.len() == before {
                return Err(format!("{} tidak ada di daftar park", dir.display()));
            }
            cfg.save(&home)?;
            println!("✓ {} tidak di-park lagi", dir.display());
        }
        "parked" | "paths" => {
            if cfg.paths.is_empty() {
                println!("(belum ada folder park — jalankan: hl park <folder>)");
            }
            for p in &cfg.paths {
                println!("{p}");
            }
        }

        "link" => {
            let dir = cwd();
            let name = norm(&pos.first().cloned().unwrap_or_else(|| {
                dir.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
            }));
            if name.is_empty()
                || name.starts_with('-')
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            {
                return Err(format!("nama situs '{name}' tidak valid (pakai huruf, angka, '-' atau '.')"));
            }
            cfg.links.insert(name.clone(), clean_abs(&dir));
            if flag(&rest, "--secure") && !cfg.secured.contains(&name) {
                cfg.secured.push(name.clone());
            }
            cfg.save(&home)?;
            println!("✓ {} → {}", cfg.url(&name), dir.display());
            hosts_hint(&cfg);
        }
        "unlink" => {
            let name = match pos.first() {
                Some(n) => norm(n),
                None => {
                    let k = util::path_key(Path::new(&clean_abs(&cwd())));
                    cfg.links
                        .iter()
                        .find(|(_, p)| util::path_key(Path::new(p)) == k)
                        .map(|(n, _)| n.clone())
                        .ok_or("folder ini tidak di-link")?
                }
            };
            if cfg.links.remove(&name).is_none() {
                return Err(format!("link '{name}' tidak ada"));
            }
            cfg.save(&home)?;
            println!("✓ link {name} dihapus");
        }
        "links" => {
            for (n, p) in &cfg.links {
                println!("{:<28} {}", cfg.host(n), p);
            }
        }
        "sites" => {
            let sites = cfg.sites();
            println!("{:<34} {:<5} {:<5} {}", "URL", "PHP", "JENIS", "FOLDER");
            for s in &sites {
                let php = cfg.php_for(&home, &s.name).unwrap_or_else(|| "-".into());
                let iso = if cfg.isolated.contains_key(&s.name) { "*" } else { "" };
                println!(
                    "{:<34} {:<5} {:<5} {}",
                    cfg.url(&s.name),
                    format!("{php}{iso}"),
                    if s.kind == Kind::Linked { "link" } else { "park" },
                    s.path.display()
                );
            }
            println!("{} situs. (* = versi PHP di-isolate)", sites.len());
        }
        "open" => {
            let url = match pos.first() {
                Some(n) => cfg.url(&norm(n)),
                None => match cfg.site_for_dir(&cwd()) {
                    Some(s) => cfg.url(&s.name),
                    None => format!("http://herdlite.{}{}", cfg.tld, if cfg.http_port == 80 { String::new() } else { format!(":{}", cfg.http_port) }),
                },
            };
            println!("Membuka {url}");
            let _ = Command::new(win::sys("rundll32.exe")).args(["url.dll,FileProtocolHandler", &url]).spawn();
        }

        "use" => {
            let v = pos.first().and_then(|v| parse_ver(v)).ok_or("contoh: hl use 8.4")?;
            if !cfg.installed_php(&home).contains(&v) {
                return Err(format!("PHP {v} belum terpasang. Jalankan: hl php:install {v}"));
            }
            cfg.default_php = v.clone();
            cfg.save(&home)?;
            phpinstall::write_shims(&home, &cfg)?;
            println!("✓ PHP global sekarang {v} (php.cmd di {}\\bin)", home.display());
        }
        "isolate" => {
            let v = pos.first().and_then(|v| parse_ver(v)).ok_or("contoh: hl isolate 8.4")?;
            let site = target_site(&cfg, &rest)?;
            if !cfg.installed_php(&home).contains(&v) {
                println!("! PHP {v} belum terpasang — jalankan: hl php:install {v}");
            }
            cfg.isolated.insert(site.clone(), v.clone());
            cfg.save(&home)?;
            println!("✓ {} sekarang memakai PHP {v}", cfg.host(&site));
        }
        "unisolate" => {
            let site = target_site(&cfg, &rest)?;
            cfg.isolated.remove(&site);
            cfg.save(&home)?;
            println!("✓ {} kembali memakai PHP global", cfg.host(&site));
        }
        "isolated" => {
            for (s, v) in &cfg.isolated {
                println!("{:<28} {v}", cfg.host(s));
            }
        }
        "which-php" => {
            let site = cfg.site_for_dir(&cwd());
            let v = match &site {
                Some(s) => cfg.php_for(&home, &s.name),
                None => cfg.global_php(&home),
            }
            .ok_or("belum ada PHP terpasang")?;
            println!(
                "PHP {v} ({}) → {}",
                site.map(|s| format!("situs {}", s.name)).unwrap_or_else(|| "global".into()),
                cfg.php_dir(&home, &v).join("php.exe").display()
            );
        }
        "php:list" => {
            let g = cfg.global_php(&home);
            let list = cfg.installed_php(&home);
            if list.is_empty() {
                println!("(belum ada — jalankan: hl php:install 8.3)");
            }
            for v in list {
                println!("{} {v:<5} {}", if Some(&v) == g.as_ref() { "*" } else { " " }, cfg.php_dir(&home, &v).display());
            }
        }
        "php:available" => {
            for r in phpinstall::available()? {
                println!("{:<8} {}", r.version, r.zip);
            }
        }
        "php:install" => {
            let v = pos.first().and_then(|v| parse_ver(v)).ok_or("contoh: hl php:install 8.3")?;
            let full = phpinstall::install(&home, &v, flag(&rest, "--force"))?;
            cfg.php_dirs.remove(&v);
            if cfg.default_php.is_empty() {
                cfg.default_php = v.clone();
            }
            cfg.save(&home)?;
            phpinstall::write_shims(&home, &cfg)?;
            println!("✓ PHP {full} terpasang di {}", home.join("php").join(&v).display());
        }
        "php:add" => {
            let v = pos.first().and_then(|v| parse_ver(v)).ok_or("contoh: hl php:add 8.3 D:\\php83")?;
            let dir = PathBuf::from(pos.get(1).ok_or("folder PHP belum diisi")?);
            if !dir.join("php-cgi.exe").is_file() {
                return Err(format!("{} tidak berisi php-cgi.exe", dir.display()));
            }
            cfg.php_dirs.insert(v.clone(), clean_abs(&dir));
            cfg.save(&home)?;
            phpinstall::write_shims(&home, &cfg)?;
            println!("✓ PHP {v} didaftarkan dari {}", dir.display());
        }
        "composer:install" => {
            let p = phpinstall::install_composer(&home)?;
            phpinstall::write_shims(&home, &cfg)?;
            println!("✓ composer.phar di {}", p.display());
        }
        "laravel:install" => {
            let p = phpinstall::install_laravel(&home, &cfg)?;
            println!("✓ Laravel installer terpasang ({}), shim laravel.cmd di {}\\bin", p.display(), home.display());
        }
        "node:install" => {
            let spec = pos.first().ok_or("contoh: hl node:install 26  atau  hl node:install lts")?;
            let v = node::install(&home, spec)?;
            if node::active(&home).is_none() {
                node::use_version(&home, &v)?;
                println!("✓ Node {v} terpasang dan aktif ({})", node::current_link(&home).display());
            } else {
                println!("✓ Node {v} terpasang. Aktifkan: hl node:use {v}");
            }
        }
        "node:use" => {
            let v = node::use_version(&home, pos.first().ok_or("contoh: hl node:use 26")?)?;
            println!("✓ Node aktif sekarang {v}");
        }
        "node:list" => {
            let act = node::active(&home);
            let list = node::installed(&home);
            if list.is_empty() {
                println!("(belum ada — jalankan: hl node:install lts)");
            }
            for v in list {
                println!("{} {v}", if Some(&v) == act.as_ref() { "*" } else { " " });
            }
        }
        "doctor" => doctor(&home, &cfg),

        "secure" | "unsecure" => {
            let on = cmd == "secure";
            if flag(&rest, "--all") {
                cfg.secure_all = on;
                cfg.save(&home)?;
                println!("✓ HTTPS untuk semua situs: {}", if on { "dipaksa" } else { "tidak dipaksa" });
            } else {
                let site = match pos.first() {
                    Some(n) => norm(n),
                    None => cfg.site_for_dir(&cwd()).map(|s| s.name).ok_or("jalankan dari folder situs atau sebutkan namanya")?,
                };
                cfg.secured.retain(|s| *s != site);
                if on {
                    cfg.secured.push(site.clone());
                }
                cfg.save(&home)?;
                println!("✓ {}", cfg.url(&site));
            }
            if on {
                let (_, created) = tls::Ca::load_or_create(&home, &cfg.tld)?;
                if created || !ca_trusted() {
                    println!("  Supaya browser tidak memberi peringatan, jalankan sekali: hl trust");
                }
            }
        }
        "trust" => trust_ca(&home, &cfg)?,
        "untrust" => {
            let st = Command::new(win::sys("certutil.exe")).args(["-user", "-delstore", "Root", &tls::ca_name()]).status().map_err(|e| e.to_string())?;
            if !st.success() {
                return Err("certutil gagal".into());
            }
            println!("✓ CA herdlite dihapus dari CurrentUser\\Root");
        }
        "ca:renew" => {
            // Buat CA baru (dengan batasan terbaru) lalu ganti yang lama di store Windows.
            println!("Mengganti CA herdlite. Windows akan menampilkan dua dialog: hapus CA lama, lalu pasang CA baru. Klik Yes pada keduanya.");
            let was_running = running_pid(&home).is_some();
            stop(&home, false);
            let _ = Command::new(win::sys("certutil.exe"))
                .args(["-user", "-delstore", "Root", &tls::ca_name()])
                .stdout(Stdio::null())
                .status();
            let dir = tls::cert_dir(&home);
            for f in ["herdlite-ca.crt", "herdlite-ca.key", "herdlite-ca.json"] {
                let _ = std::fs::remove_file(dir.join(f));
            }
            trust_ca(&home, &cfg)?;
            if was_running {
                start(&home, &cfg)?;
            }
            println!("✓ CA baru aktif (khusus .{} dan localhost, tanpa alamat IP, hanya untuk server web)", cfg.tld);
        }

        "hosts" => {
            if pos.first().map(|s| s.as_str()) == Some("sync") {
                hosts_write(&cfg, HostsMode::Sync, flag(&rest, "--elevated"))?;
            } else if pos.first().map(|s| s.as_str()) == Some("clear") {
                hosts_write(&cfg, HostsMode::Clear, flag(&rest, "--elevated"))?;
            } else {
                let miss = hosts::missing(&cfg)?;
                if miss.is_empty() {
                    println!("✓ semua nama .{} sudah ada di hosts", cfg.tld);
                } else {
                    println!("{} nama belum ada di hosts:", miss.len());
                    for h in &miss {
                        println!("  {h}");
                    }
                    println!("Jalankan: hl hosts sync  (akan muncul jendela UAC, klik Yes)");
                }
            }
        }
        "import-herd" => {
            let r = herdimport::read()?;
            herdimport::apply(&mut cfg, &r, flag(&rest, "--use-herd-php"));
            cfg.save(&home)?;
            println!("✓ Diimpor dari Herd ({}):", herdimport::herd_home().display());
            println!("  park     : {}", if r.paths.is_empty() { "-".into() } else { r.paths.join(", ") });
            println!("  link     : {}", r.links.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));
            println!("  isolate  : {}", r.isolated.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join(", "));
            println!("  secure   : {}", if r.secured.is_empty() { "-".into() } else { r.secured.join(", ") });
            println!("  PHP global: {}", r.default_php.unwrap_or_else(|| "-".into()));
            if flag(&rest, "--use-herd-php") {
                println!("  PHP Herd dipakai langsung: {}", r.herd_php.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>().join(", "));
                phpinstall::write_shims(&home, &cfg)?;
            } else {
                println!("  (PHP tidak diimpor; pasang sendiri: hl php:install 8.3, atau ulangi dengan --use-herd-php)");
            }
        }
        "config" => {
            let mut v = serde_json::to_value(&cfg).map_err(|e| e.to_string())?;
            match (pos.first(), pos.get(1)) {
                (None, _) => println!("{}\n\n({})", serde_json::to_string_pretty(&v).unwrap(), config::config_path(&home).display()),
                (Some(k), None) => println!("{}", v.get(k).map(|x| x.to_string()).unwrap_or_else(|| "(tidak ada)".into())),
                (Some(k), Some(val)) => {
                    if v.get(k).is_none() {
                        return Err(format!("kunci '{k}' tidak dikenal"));
                    }
                    v[k] = serde_json::from_str(val).unwrap_or_else(|_| serde_json::Value::String(val.clone()));
                    let new: Config = serde_json::from_value(v).map_err(|e| format!("nilai tidak valid: {e}"))?;
                    new.save(&home)?;
                    println!("✓ {k} = {val}");
                    if ["http_port", "https_port", "bind", "tld"].contains(&k.as_str()) {
                        println!("  Berlaku setelah: hl restart");
                    }
                }
            }
        }
        "path:add" => path_add(&home)?,
        "autostart" => match pos.first().map(|s| s.as_str()) {
            Some("on") => autostart(true)?,
            Some("off") => autostart(false)?,
            _ => return Err("pakai: hl autostart on|off".into()),
        },
        "shortcut" => shortcut(&home, flag(&rest, "--desktop"))?,
        "setup" => setup(&home)?,
        "uninstall" => uninstall(&home, flag(&rest, "--yes"))?,
        "lock" => lock_permissions(&home)?,
        other => return Err(format!("perintah tidak dikenal: {other}  (lihat: hl help)")),
    }
    Ok(0)
}

fn run_quiet(prog: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(win::sys(&format!("{prog}.exe"))).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn ca_trusted() -> bool {
    Command::new(win::sys("certutil.exe"))
        .args(["-user", "-verifystore", "Root", &tls::ca_name()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn target_site(cfg: &Config, rest: &[String]) -> Result<String, String> {
    if let Some(s) = opt(rest, "--site") {
        return Ok(norm(&s));
    }
    cfg.site_for_dir(&cwd())
        .map(|s| s.name)
        .ok_or_else(|| "folder ini bukan situs herdlite; jalankan dari folder situs atau pakai --site=nama".into())
}

fn hosts_hint(cfg: &Config) {
    if let Ok(m) = hosts::missing(cfg) {
        if !m.is_empty() {
            println!("  ! {} nama .{} belum ada di hosts file. Jalankan: hl hosts sync (klik Yes pada UAC)", m.len(), cfg.tld);
        }
    }
}

fn php_passthru(home: &Path, args: &[String]) -> Result<i32, String> {
    let cfg = Config::load(home)?;
    let v = match cfg.site_for_dir(&cwd()) {
        Some(s) => cfg.php_for(home, &s.name),
        None => cfg.global_php(home),
    }
    .ok_or("belum ada PHP terpasang (hl php:install 8.3)")?;
    let exe = cfg.php_dir(home, &v).join("php.exe");
    let st = Command::new(&exe).args(args).status().map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(st.code().unwrap_or(1))
}

fn daemon_exe() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let d = me.with_file_name("herdlited.exe");
    if d.is_file() {
        Ok(d)
    } else {
        Err(format!("herdlited.exe tidak ditemukan di sebelah {}", me.display()))
    }
}

fn read_pid(home: &Path) -> Option<u32> {
    std::fs::read_to_string(server::pid_path(home)).ok()?.trim().parse().ok()
}

fn running_pid(home: &Path) -> Option<u32> {
    read_pid(home).filter(|p| win::pid_is(*p, "herdlited.exe"))
}

fn port_free(bind: &str, port: u16) -> Result<(), String> {
    match std::net::TcpListener::bind((bind, port)) {
        Ok(_) => Ok(()),
        Err(e) => {
            let who = win::port_owner(port).unwrap_or_else(|| "proses lain".into());
            Err(format!(
                "port {port} sedang dipakai oleh {who} ({e}).\n  Kalau itu nginx milik Herd, tutup Herd dulu, atau pakai port lain: hl config {} {}",
                if port == 443 { "https_port" } else { "http_port" },
                if port == 443 { 8443 } else { 8080 }
            ))
        }
    }
}

fn start(home: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(pid) = running_pid(home) {
        println!("herdlite sudah berjalan (pid {pid})");
        return Ok(());
    }
    port_free(&cfg.bind, cfg.http_port)?;
    if cfg.https_port != 0 {
        port_free(&cfg.bind, cfg.https_port)?;
        tls::Ca::load_or_create(home, &cfg.tld)?;
    }
    if cfg.global_php(home).is_none() {
        println!("! Belum ada PHP terpasang. Situs PHP baru bisa jalan setelah: hl php:install 8.3");
    }
    let exe = daemon_exe()?;
    let _ = std::fs::remove_file(server::pid_path(home));
    win::no_inherit_std_handles();
    let flags =win::DETACHED_PROCESS | win::CREATE_NEW_PROCESS_GROUP | win::CREATE_NO_WINDOW;
    let spawn = |extra: u32| {
        Command::new(&exe)
            .env("HERDLITE_HOME", home)
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags | extra)
            .spawn()
    };
    // Lepas dari job object terminal (kalau diizinkan) supaya server tidak ikut mati saat terminal ditutup.
    let child = spawn(win::CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(0)).map_err(|e| format!("gagal menjalankan {}: {e}", exe.display()))?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if read_pid(home) == Some(pid) && TcpStream::connect((cfg.bind.as_str(), cfg.http_port)).is_ok() {
            break;
        }
        if !win::pid_alive(pid) || Instant::now() > deadline {
            let log = std::fs::read_to_string(home.join("logs").join("herdlite.log")).unwrap_or_default();
            let last = log.lines().last().unwrap_or("").to_string();
            return Err(format!("server gagal menyala. Log terakhir: {last}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let suffix = if cfg.http_port == 80 { String::new() } else { format!(":{}", cfg.http_port) };
    println!("✓ herdlite berjalan (pid {pid})");
    println!("  Dashboard: http://herdlite.{}{suffix}  ·  {} situs", cfg.tld, cfg.sites().len());
    hosts_hint(cfg);
    Ok(())
}

fn stop(home: &Path, verbose: bool) {
    match running_pid(home) {
        Some(pid) => {
            win::kill_tree(pid);
            let t = Instant::now();
            while win::pid_alive(pid) && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = std::fs::remove_file(server::pid_path(home));
            println!("✓ herdlite dihentikan");
        }
        None => {
            if verbose {
                println!("herdlite tidak sedang berjalan");
            }
        }
    }
}

fn http_get_status(cfg: &Config) -> Option<serde_json::Value> {
    let mut s = TcpStream::connect_timeout(&format!("{}:{}", if cfg.bind == "0.0.0.0" { "127.0.0.1" } else { &cfg.bind }, cfg.http_port).parse().ok()?, Duration::from_secs(2)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(s, "GET /__herdlite/status HTTP/1.1\r\nHost: herdlite.{}\r\nConnection: close\r\n\r\n", cfg.tld).ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    let body = buf.split_once("\r\n\r\n")?.1;
    serde_json::from_str(body).ok()
}

fn status(home: &Path, cfg: &Config) {
    println!("herdlite {}  ·  home: {}", herdlite::VERSION, home.display());
    match running_pid(home) {
        Some(pid) => {
            println!("Server   : berjalan (pid {pid}), http :{}{}", cfg.http_port, if cfg.https_port != 0 { format!(", https :{}", cfg.https_port) } else { String::new() });
            if let Some(v) = http_get_status(cfg) {
                let list: Vec<String> = v["php_running"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|p| format!("{} (pid {}, port {}, idle {}s)", p["version"].as_str().unwrap_or("?"), p["pid"], p["port"], p["idle_secs"]))
                    .collect();
                println!("php-cgi  : {}", if list.is_empty() { "tidak ada yang jalan (dinyalakan saat ada request)".into() } else { list.join(", ") });
            }
        }
        None => println!("Server   : mati  (hl start)"),
    }
    println!("PHP      : global {}  ·  terpasang: {}", cfg.global_php(home).unwrap_or_else(|| "-".into()), cfg.installed_php(home).join(", "));
    println!("Situs    : {} ({} folder park, {} link, {} isolate)", cfg.sites().len(), cfg.paths.len(), cfg.links.len(), cfg.isolated.len());
}

/// Apakah path (setelah symlink/junction diikuti) masih menunjuk ke folder Herd?
fn from_herd(p: &Path) -> bool {
    let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let k = util::path_key(&real);
    let raw = util::path_key(p);
    [r"\.config\herd\", r"\config~1\herd\", r"\herd-data\", r"\programs\herd\"]
        .iter()
        .any(|m| k.contains(m) || raw.contains(m))
}

fn where_first(cmd: &str) -> Option<PathBuf> {
    let out = Command::new(win::sys("where.exe")).arg(cmd).stderr(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(|l| PathBuf::from(l.trim())).filter(|p| !p.as_os_str().is_empty())
}

fn doctor(home: &Path, cfg: &Config) {
    let mark = |b: bool| if b { "✓" } else { "✗" };
    println!("Cek kesiapan pindah dari Herd ke herdlite ({})\n", home.display());

    let php = cfg.global_php(home);
    println!("{} PHP herdlite        : {}", mark(php.is_some()), php.clone().unwrap_or_else(|| "belum ada → hl php:install 8.3".into()));
    let comp = home.join("bin").join("composer.phar").is_file();
    println!("{} Composer herdlite   : {}", mark(comp), if comp { "ada" } else { "belum → hl composer:install" });
    let lara = phpinstall::laravel_bat();
    println!("{} Laravel installer   : {}", mark(lara.is_some()), lara.map(|p| p.display().to_string()).unwrap_or_else(|| "belum → hl laravel:install".into()));
    let nd = node::active(home);
    println!("{} Node herdlite       : {}", mark(nd.is_some()), nd.unwrap_or_else(|| "belum → hl node:install 26".into()));

    println!("\nPerintah di PATH (terminal ini):");
    for c in ["php", "composer", "laravel", "node", "npm"] {
        match where_first(c) {
            Some(p) => {
                let herd = from_herd(&p);
                println!("{} {c:<9} → {}{}", mark(!herd), p.display(), if herd { "  (masih dari Herd)" } else { "" });
            }
            None => println!("✗ {c:<9} → tidak ditemukan"),
        }
    }

    println!("\nLain-lain:");
    match hosts::missing(cfg) {
        Ok(m) if m.is_empty() => println!("✓ hosts file        : semua nama .{} sudah ada", cfg.tld),
        Ok(m) => println!("✗ hosts file        : {} nama belum ada → hl hosts sync (klik Yes pada UAC)", m.len()),
        Err(e) => println!("✗ hosts file        : {e}"),
    }
    for (label, port) in [("port HTTP ", cfg.http_port), ("port HTTPS", cfg.https_port)] {
        if port == 0 {
            continue;
        }
        let ours = running_pid(home).is_some();
        match std::net::TcpListener::bind((cfg.bind.as_str(), port)) {
            Ok(_) => println!("✓ {label}        : {port} bebas"),
            Err(_) => {
                let who = win::port_owner(port).unwrap_or_else(|| "proses lain".into());
                let me = ours && who.to_lowercase().starts_with("herdlited");
                println!("{} {label}        : {port} dipakai {who}", mark(me));
            }
        }
    }
    if cfg.http_port != 80 {
        println!("! port HTTP herdlite : {} (bukan 80) → setelah Herd ditutup: hl config http_port 80 && hl config https_port 443 && hl restart", cfg.http_port);
    }
    println!("{} CA HTTPS dipercaya  : {}", mark(ca_trusted()), if ca_trusted() { "ya" } else { "belum → hl trust" });
    let herd_running = Command::new(win::sys("tasklist.exe"))
        .args(["/FI", "IMAGENAME eq Herd.exe", "/NH"])
        .creation_flags(win::CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Herd.exe"))
        .unwrap_or(false);
    println!("{} Herd               : {}", mark(!herd_running), if herd_running { "masih berjalan" } else { "tidak berjalan" });
}

fn trust_ca(home: &Path, cfg: &Config) -> Result<(), String> {
    tls::Ca::load_or_create(home, &cfg.tld)?;
    if ca_trusted() {
        println!("✓ CA herdlite sudah dipercaya");
        return Ok(());
    }
    let crt = tls::ca_crt_path(home);
    println!("Memasang {} ke CurrentUser\\Root (klik Yes pada dialog Windows)…", crt.display());
    let st = Command::new(win::sys("certutil.exe"))
        .args(["-user", "-addstore", "Root"])
        .arg(&crt)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() || !ca_trusted() {
        return Err("certutil gagal / dibatalkan".into());
    }
    println!("✓ CA dipercaya. Chrome/Edge langsung ikut; Firefox: aktifkan security.enterprise_roots.enabled.");
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum HostsMode {
    Sync,
    Clear,
}

/// Tulis (sync) atau hapus (clear) blok herdlite di hosts. Kalau bukan Administrator,
/// minta izin lewat jendela UAC (proses hl kedua yang ter-elevasi), lalu periksa hasilnya.
fn hosts_write(cfg: &Config, mode: HostsMode, elevated: bool) -> Result<(), String> {
    let res = match mode {
        HostsMode::Sync => hosts::sync(cfg).map(|n| format!("hosts diperbarui ({n} nama .{} → {})", cfg.tld, cfg.bind)),
        HostsMode::Clear => hosts::clear().map(|_| "blok herdlite dihapus dari hosts".to_string()),
    };
    match res {
        Ok(msg) => {
            println!("✓ {msg}");
            Ok(())
        }
        Err(e) if !elevated && e.starts_with("butuh hak Administrator") => {
            println!("Meminta izin Administrator untuk mengubah hosts (klik Yes pada jendela UAC)…");
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            let sub = if mode == HostsMode::Sync { "sync" } else { "clear" };
            let ps = format!(
                "try {{ $p = Start-Process -FilePath '{}' -ArgumentList 'hosts','{sub}','--elevated' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode }} catch {{ exit 1223 }}",
                me.display().to_string().replace('\'', "''")
            );
            let st = Command::new(win::sys("powershell.exe")).args(["-NoProfile", "-Command", &ps]).status().map_err(|e| e.to_string())?;
            if st.code() == Some(1223) {
                return Err("dibatalkan di jendela UAC".into());
            }
            match mode {
                HostsMode::Sync => {
                    let miss = hosts::missing(cfg)?;
                    if miss.is_empty() {
                        println!("✓ hosts diperbarui ({} nama .{})", hosts::wanted(cfg).len(), cfg.tld);
                        Ok(())
                    } else {
                        Err(format!("hosts belum lengkap, {} nama belum ada", miss.len()))
                    }
                }
                HostsMode::Clear => {
                    if hosts::has_block()? {
                        Err("blok herdlite masih ada di hosts".into())
                    } else {
                        println!("✓ blok herdlite dihapus dari hosts");
                        Ok(())
                    }
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// Hapus folder herdlite dari PATH user.
fn path_remove(home: &Path) -> Result<(), String> {
    let dirs = [home.join("bin"), node::current_link(home)];
    let list = dirs.iter().map(|d| format!("'{}'", d.display().to_string().replace('\'', "''"))).collect::<Vec<_>>().join(",");
    let ps = format!(
        "$rm=@({list}); $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not $p){{$p=''}}; \
         $keep=@($p -split ';' | Where-Object {{ $_ -and ($rm -notcontains $_) }}); \
         [Environment]::SetEnvironmentVariable('Path', ($keep -join ';'), 'User'); 'ok'"
    );
    let out = Command::new(win::sys("powershell.exe")).args(["-NoProfile", "-Command", &ps]).output().map_err(|e| e.to_string())?;
    if String::from_utf8_lossy(&out.stdout).trim() != "ok" {
        return Err(format!("gagal mengubah PATH: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    println!("✓ herdlite dihapus dari PATH user");
    Ok(())
}

/// Bersihkan semua perubahan sistem yang dibuat herdlite. File program dihapus oleh
/// uninstaller (atau manual); folder proyek TIDAK pernah disentuh.
fn uninstall(home: &Path, yes: bool) -> Result<(), String> {
    let cfg = Config::load(home).unwrap_or_default();
    if !yes {
        println!("Ini akan menghentikan herdlite dan membersihkan autostart, shortcut, PATH, CA HTTPS, dan hosts.");
        println!("Folder proyek ({}) TIDAK dihapus.", if cfg.paths.is_empty() { "-".into() } else { cfg.paths.join(", ") });
        print!("Lanjutkan? (y/N) ");
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
        if !s.trim().eq_ignore_ascii_case("y") {
            return Err("dibatalkan".into());
        }
    }
    stop(home, false);
    let mut errs = Vec::new();
    let mut step = |name: &str, r: Result<(), String>| {
        if let Err(e) = r {
            println!("✗ {name}: {e}");
            errs.push(name.to_string());
        }
    };
    step("autostart", autostart(false));
    for f in ["Programs", "Desktop"] {
        let ps = format!("$l=Join-Path ([Environment]::GetFolderPath('{f}')) 'herdlite.lnk'; if(Test-Path $l){{ Remove-Item $l -Force }}");
        let _ = Command::new(win::sys("powershell.exe")).args(["-NoProfile", "-Command", &ps]).status();
    }
    println!("✓ shortcut dihapus");
    step("PATH", path_remove(home));
    if ca_trusted() {
        println!("Menghapus CA herdlite dari Windows (klik Yes pada dialog)…");
        let st = Command::new(win::sys("certutil.exe"))
            .args(["-user", "-delstore", "Root", &tls::ca_name()])
            .stdout(Stdio::null())
            .status()
            .map_err(|e| e.to_string());
        step("CA", st.and_then(|s| if s.success() { Ok(()) } else { Err("certutil gagal / dibatalkan".into()) }));
    }
    if hosts::has_block().unwrap_or(false) {
        step("hosts", hosts_write(&cfg, HostsMode::Clear, false));
    }
    if errs.is_empty() {
        println!("✓ herdlite sudah dibersihkan dari sistem. Folder proyekmu tetap utuh.");
        Ok(())
    } else {
        Err(format!("sebagian gagal: {}", errs.join(", ")))
    }
}

fn path_add(home: &Path) -> Result<(), String> {
    // Taruh di depan PATH user supaya php/composer/node milik herdlite yang dipakai.
    let dirs = [home.join("bin"), node::current_link(home)];
    let list = dirs.iter().map(|d| format!("'{}'", d.display().to_string().replace('\'', "''"))).collect::<Vec<_>>().join(",");
    let ps = format!(
        "$add=@({list}); $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not $p){{$p=''}}; \
         $rest=@($p -split ';' | Where-Object {{ $_ -and ($add -notcontains $_) }}); \
         [Environment]::SetEnvironmentVariable('Path', (($add + $rest) -join ';'), 'User'); 'ok'"
    );
    let out = Command::new(win::sys("powershell.exe")).args(["-NoProfile", "-Command", &ps]).output().map_err(|e| e.to_string())?;
    if String::from_utf8_lossy(&out.stdout).trim() != "ok" {
        return Err(format!("gagal mengubah PATH: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    for d in &dirs {
        println!("✓ {} ada di depan PATH user", d.display());
    }
    println!("  Buka terminal baru supaya berlaku.");
    Ok(())
}

fn autostart(on: bool) -> Result<(), String> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    if on {
        let exe = daemon_exe()?;
        run_quiet("reg", &["add", key, "/v", "herdlite", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"])?;
        println!("✓ herdlite menyala otomatis saat login");
    } else {
        let _ = run_quiet("reg", &["delete", key, "/v", "herdlite", "/f"]);
        println!("✓ autostart dimatikan");
    }
    Ok(())
}

/// Shortcut "herdlite" di Start Menu (dan opsional Desktop): klik = nyalakan server + buka dashboard.
fn shortcut(home: &Path, desktop: bool) -> Result<(), String> {
    let exe = daemon_exe()?;
    let mut folders = vec!["Programs"];
    if desktop {
        folders.push("Desktop");
    }
    for f in folders {
        let ps = format!(
            "$l=Join-Path ([Environment]::GetFolderPath('{f}')) 'herdlite.lnk'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut($l); \
             $s.TargetPath='{exe}'; $s.Arguments='--open'; $s.WorkingDirectory='{home}'; \
             $s.Description='herdlite: server PHP lokal (situs .test)'; $s.Save(); $l",
            exe = exe.display().to_string().replace('\'', "''"),
            home = home.display().to_string().replace('\'', "''"),
        );
        let out = Command::new(win::sys("powershell.exe")).args(["-NoProfile", "-Command", &ps]).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("gagal membuat shortcut: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        println!("✓ Shortcut: {}", String::from_utf8_lossy(&out.stdout).trim());
    }
    Ok(())
}

fn step(name: &str, r: Result<(), String>) {
    if let Err(e) = r {
        println!("✗ {name}: {e}");
    }
}

/// Satu perintah untuk menyiapkan semuanya. Langkah yang sudah beres dilewati.
fn setup(home: &Path) -> Result<(), String> {
    println!("── herdlite setup ({}) ──", home.display());
    let mut cfg = Config::load(home)?;

    println!("\n[1/8] PHP, Composer, Laravel installer");
    if cfg.installed_php(home).is_empty() {
        step("PHP", phpinstall::install(home, "8.3", false).map(|v| println!("✓ PHP {v} terpasang")));
        cfg = Config::load(home)?;
        if cfg.default_php.is_empty() {
            cfg.default_php = "8.3".into();
            cfg.save(home)?;
        }
    } else {
        println!("✓ PHP {}", cfg.installed_php(home).join(", "));
    }
    if !home.join("bin").join("composer.phar").is_file() {
        step("Composer", phpinstall::install_composer(home).map(|_| println!("✓ Composer terpasang")));
    } else {
        println!("✓ Composer");
    }
    if phpinstall::laravel_bat().is_none() {
        step("Laravel installer", phpinstall::install_laravel(home, &cfg).map(|_| println!("✓ Laravel installer terpasang")));
    } else {
        println!("✓ Laravel installer");
    }
    step("shim", phpinstall::write_shims(home, &cfg));

    println!("\n[2/8] Node.js");
    match node::active(home) {
        Some(v) => println!("✓ Node {v}"),
        None => step(
            "Node",
            node::install(home, "lts")
                .and_then(|v| node::use_version(home, &v))
                .map(|v| println!("✓ Node {v} terpasang dan aktif")),
        ),
    }

    println!("\n[3/8] PATH user");
    step("PATH", path_add(home));

    println!("\n[4/8] Port 80/443");
    if cfg.http_port != 80 || cfg.https_port != 443 {
        stop(home, false);
        let free80 = port_free(&cfg.bind, 80).is_ok();
        let free443 = port_free(&cfg.bind, 443).is_ok();
        if free80 && free443 {
            cfg.http_port = 80;
            cfg.https_port = 443;
            cfg.save(home)?;
            println!("✓ herdlite memakai port 80 dan 443");
        } else {
            let who = win::port_owner(if free80 { 443 } else { 80 }).unwrap_or_else(|| "proses lain".into());
            println!(
                "✗ port 80/443 masih dipakai {who}. Tutup dulu (mis. Herd), lalu jalankan hl setup lagi. Sementara tetap di {}/{}.",
                cfg.http_port, cfg.https_port
            );
        }
    } else {
        println!("✓ port 80 dan 443");
    }

    println!("\n[5/8] Server");
    step("start", start(home, &cfg));

    println!("\n[6/8] HTTPS (CA lokal)");
    step("trust", trust_ca(home, &cfg));

    println!("\n[7/8] hosts file");
    match hosts::missing(&cfg) {
        Ok(m) if m.is_empty() => println!("✓ semua nama .{} sudah ada", cfg.tld),
        _ => step("hosts", hosts_write(&cfg, HostsMode::Sync, false)),
    }

    println!("\n[8/8] Autostart dan shortcut Start Menu");
    step("autostart", autostart(true));
    step("shortcut", shortcut(home, false));
    step("izin folder", lock_permissions(home));

    println!();
    doctor(home, &cfg);
    Ok(())
}

/// Kunci izin folder herdlite: hanya akun ini, SYSTEM, dan Administrators yang boleh menulis
/// (folder di drive D biasanya mewarisi "Authenticated Users: Modify"). Kunci CA hanya bisa
/// dibaca akun ini. Mencegah akun/proses lain mengganti hl.exe yang dijalankan lewat UAC,
/// mengubah config.json, atau membaca kunci CA.
fn lock_permissions(home: &Path) -> Result<(), String> {
    let me = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").map_err(|_| "USERNAME tidak diketahui")?
    );
    let full = format!("{me}:(OI)(CI)F");
    let icacls = |path: &Path, extra: &[&str]| -> Result<(), String> {
        let mut args: Vec<String> = vec![path.display().to_string(), "/inheritance:r".into(), "/grant:r".into()];
        args.extend(["*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F", full.as_str()].iter().map(|s| s.to_string()));
        args.extend(extra.iter().map(|s| s.to_string()));
        args.push("/Q".into());
        let out = Command::new(win::sys("icacls.exe")).args(&args).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    };
    // Users tetap boleh membaca & menjalankan (php.exe, node.exe), tapi tidak menulis.
    icacls(home, &["*S-1-5-32-545:(OI)(CI)RX"])?;
    let certs = tls::cert_dir(home);
    if certs.is_dir() {
        icacls(&certs, &[])?;
    }
    println!("✓ Izin folder dikunci: hanya {me}, SYSTEM, dan Administrators yang bisa menulis; kunci CA hanya untuk {me}");
    Ok(())
}
