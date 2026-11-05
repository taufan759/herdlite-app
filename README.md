# herdlite

Pengganti **Laravel Herd** yang sangat ringan untuk Windows. Cara pakainya mirip Herd/Valet:

- situs dibuka lewat `http://<folder>.test`
- **park** folder (setiap subfolder otomatis jadi situs) dan **link** satu folder
- versi PHP **global** dan **per situs** (`isolate`)
- **HTTPS lokal** dengan HTTP/2 dan sertifikat yang dibuat otomatis per situs

Bedanya, herdlite tidak memakai Electron, nginx, Node, atau layanan Windows. Isinya dua exe kecil ditambah PHP resmi dari windows.php.net.

| | Laravel Herd 2.0.3 | herdlite 0.1 |
|---|---|---|
| Ukuran program | 1.265 MB (Electron) + 1.358 MB data/bin | **2,5 MB** (`hl.exe` 0,9 MB + `herdlited.exe` 1,7 MB) |
| Total dengan 2 versi PHP | ± 2,7 GB | **182 MB** (PHP 8.3 88 MB + 8.4 92 MB) |
| RAM saat aktif (1 versi PHP) | ± 980 MB private, 26 proses | **± 41 MB** private, 4 proses |
| RAM saat menganggur | sama (semuanya tetap jalan) | **1,3 MB**, 1 proses (php-cgi dimatikan otomatis) |
| Butuh admin / service Windows | ya (HerdHelper, Automatic) | tidak (hosts file opsional, lihat di bawah) |

*Diukur di laptop yang sama pada 5 Nov 2026 dengan situs Laravel 11 yang sama. Kecepatan per request setara (0,25–0,45 dtk untuk halaman yang sama, Herd 0,38–0,52 dtk).*

## Cara kerja

```
browser ──▶ herdlited.exe (Rust: hyper + rustls, 1 proses, ~1 MB RAM)
              │  Host: toko.test → cari folder "toko" di folder park / link
              │  file statis (css/js/gambar) → dilayani langsung, dukung Range & 304
              │  *.php / route Laravel → FastCGI
              ▼
           php-cgi.exe 8.3 (port 9283)   php-cgi.exe 8.4 (port 9284) …
           dinyalakan saat request pertama, dimatikan kalau menganggur 30 menit
```

- **Tanpa konfigurasi nginx.** Situs dicari saat request datang (mirip `server.php` di Valet), jadi folder baru di folder park langsung bisa dibuka. Perubahan `park`/`link`/`isolate` langsung berlaku tanpa restart.
- **Document root:** `public\` kalau berisi `index.php`/`index.html` (Laravel, Symfony, dll.). Kalau tidak ada, root folder (PHP polos, WordPress). Request ke file yang tidak ada diteruskan ke `index.php` (front controller), sama seperti `try_files $uri $uri/ /index.php`.
- **HTTPS:** herdlite membuat CA lokal sekali (`<home>\certs`). Sertifikat tiap situs dibuat di memori saat browser pertama kali meminta (SNI), dengan SAN `situs.test` dan `*.situs.test`. CA diberi *name constraints* sehingga **hanya sah untuk `.test` dan `localhost`**. Kalau file kuncinya bocor pun, CA ini tidak bisa dipakai memalsukan situs lain.
- **Aman kalau crash:** herdlited memasukkan dirinya ke Job Object *kill-on-close*. Kalau server mati atau dibunuh, semua php-cgi ikut mati.

## Instalasi

Butuh Rust (MSVC) untuk build. Windows 10/11 sudah menyertakan `curl.exe` dan `tar.exe`, yang dipakai untuk mengunduh PHP.

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1     # default: D:\DevTools\herdlite
D:\DevTools\herdlite\bin\hl.exe path:add                 # tambahkan ke PATH user, lalu buka terminal baru
hl import-herd                                           # opsional: salin park/link/isolate dari Herd
hl php:install 8.3                                       # unduh PHP resmi + cek SHA-256
hl start
```

Folder data (`home`) adalah induk folder `bin` tempat `hl.exe` berada, atau `HERDLITE_HOME`. Isinya: `config.json`, `php\<versi>\`, `certs\`, `logs\`, `bin\` (php.cmd, php83.cmd, composer.cmd).

## Perintah

```
hl start | stop | restart | status | log
hl park [folder]        hl unpark [folder]      hl parked
hl link [nama]          hl unlink [nama]        hl links       hl sites      hl open [nama]
hl use 8.4              hl isolate 8.2 [--site=nama]           hl unisolate  hl isolated
hl php [arg…]           # php sesuai versi situs di folder sekarang (mirip `herd php`)
hl which-php            hl php:list   hl php:available   hl php:install 8.4   hl php:add 8.3 <folder>
hl composer:install
hl secure [nama|--all]  hl unsecure [nama|--all]   hl trust   hl untrust
hl hosts                hl hosts sync          # sync: jalankan dari terminal Administrator
hl config [kunci [nilai]]                      # mis. hl config http_port 8080
hl autostart on|off     hl import-herd [--use-herd-php]
```

Dashboard (hanya baca) tersedia di `http://herdlite.test`: daftar situs, versi PHP, dan php-cgi yang sedang jalan.

## Hal yang butuh izin / trade-off

| Kebutuhan | Cara herdlite | Catatan |
|---|---|---|
| `*.test` ke 127.0.0.1 | hosts file, blok `# herdlite-start … # herdlite-end` | Menulis hosts butuh **Administrator**. herdlite tidak pernah meminta elevasi sendiri; jalankan `hl hosts sync` dari terminal admin setiap ada situs baru. `hl hosts` menunjukkan nama yang belum ada. Entri lama dari Herd tetap berfungsi. |
| Wildcard tanpa edit hosts | belum dibuat | Alternatifnya DNS mini + aturan NRPT `.test` (sekali admin). Tidak dipilih karena mengubah kebijakan DNS Windows dan bisa dilewati browser yang memakai DoH. |
| Percaya CA HTTPS | `hl trust` → `certutil -user -addstore Root` | Masuk ke store **CurrentUser** (tanpa admin, ada dialog konfirmasi Windows). Firefox: aktifkan `security.enterprise_roots.enabled`. |
| Port 80/443 | bind `127.0.0.1` | Windows tidak butuh admin untuk port < 1024. **Port tidak bisa dipakai bersamaan dengan Herd**: tutup Herd dulu, atau pakai `hl config http_port 8080` / `https_port 8443`. |
| Autostart | `HKCU\…\Run` → `herdlited.exe` | Tanpa service, tanpa admin. |

Yang **tidak** disertakan (sengaja, demi ringan): GUI/tray, dump viewer, mail catcher, log viewer, Expose, Node/nvm (pakai Node yang sudah terpasang), layanan database (MySQL/Postgres tetap dijalankan sendiri), Xdebug (bisa diaktifkan manual di `php.ini`).

## Konfigurasi (`config.json`)

| Kunci | Default | Arti |
|---|---|---|
| `tld` | `test` | domain lokal |
| `http_port` / `https_port` | 80 / 443 | `https_port: 0` mematikan HTTPS |
| `default_php` | versi terbaru | PHP global |
| `php_children` | 2 | worker php-cgi per versi (PHP_FCGI_CHILDREN) |
| `php_idle_minutes` | 30 | matikan php-cgi menganggur (0 = tidak pernah) |
| `max_body_mb` | 512 | batas upload |

## Lisensi

MIT
