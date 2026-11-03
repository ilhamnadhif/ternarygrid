# TernaryGrid

**Klien database SQL yang ringan, ditulis sepenuhnya dengan Rust.**

TernaryGrid adalah alternatif native untuk DBeaver pada database SQL di macOS, Windows, dan Linux: satu
biner kecil, tanpa JVM, tanpa Electron. Terbuka dalam sekitar 0,1 detik, CPU 0 % saat diam, memakai sekitar 90 MB memori — dan
hasil query jutaan baris tidak perlu muat di memori.

[English](README.md)

![TernaryGrid: navigator, editor SQL dan grid hasil](docs/screenshots/editor.png)

## Sorotan

- **Banyak database, satu antarmuka**: PostgreSQL, MySQL, MariaDB, SQLite, SQL Server, Oracle,
  CockroachDB, ClickHouse, Cassandra / ScyllaDB, Redis, MongoDB, dan Elasticsearch / OpenSearch,
  semuanya dengan driver yang ditulis dalam Rust (tanpa Java, tanpa Oracle Client). Produk yang
  kompatibel punya entri sendiri dengan default yang sesuai: YugabyteDB, TimescaleDB, Supabase, Neon,
  Redshift, AlloyDB, Greenplum, QuestDB, CrateDB, TiDB, OceanBase, Doris, StarRocks, SingleStore,
  PlanetScale, Azure SQL Database, Amazon Keyspaces, Valkey, Dragonfly, KeyDB, MongoDB Atlas,
  FerretDB, Amazon DocumentDB, Azure Cosmos DB for MongoDB, dan Amazon OpenSearch Service.
- **Redis, MongoDB, dan Elasticsearch** di editor dan grid yang sama: perintah Redis (key per tipe di
  navigator, hash dan list besar dibaca bertahap), perintah `mongosh`
  (`db.users.find({age: {$gt: 30}})`, aggregation, filter di tab data menerima dokumen query), dan
  SQL Elasticsearch atau request gaya Kibana (`GET /logs/_search` + JSON) dengan hits sebagai baris.
- **Editor SQL** dengan autocomplete sesuai skema (tabel, kolom, join, alias, CTE), template,
  parameter `:nama` / `${nama}`, `@set` / `@include` / `@export`, pemformat, outline, penanda error,
  execution plan (teks, pohon, diagram), dan riwayat query yang bisa dicari.
- **Grid data** yang men-scroll 1 juta baris di bawah 1 ms per frame: edit langsung yang disimpan
  dalam satu transaksi, navigasi foreign key, filter dan urutan di server atau klien, tampilan
  record / JSON / XML, panel nilai, grafik, dan peta untuk kolom geometri.
- **Aman secara bawaan**: tipe koneksi development / test / production, konfirmasi untuk
  `DELETE`/`UPDATE` tanpa `WHERE` dan `DROP`, koneksi read-only, sesi terpisah per editor.
- **Koneksi** lewat SSH tunnel, proxy SOCKS5 / HTTP, dan SSL. Kata sandi disimpan di keychain sistem
  (macOS Keychain, Windows Credential Manager, GNOME Keyring / KWallet) atau brankas terenkripsi
  dengan kata sandi master — tidak pernah di file. Koneksi yang terputus saat komputer tidur atau
  jaringan hilang tersambung lagi dengan sendirinya.
- **Impor** koneksi dari DBeaver, `~/.pgpass`, dan URL.
- **Fitur lain yang biasa dipakai pengguna DBeaver**: ekspor / impor (CSV, JSON, SQL, XLSX, XML,
  Parquet …), migrasi antar database, data uji, diagram ER, query builder visual, schema compare dan
  data compare (dengan output Liquibase), desainer tabel, task dan penjadwal, backup / restore,
  session dan lock manager, dashboard server, serta debugger PostgreSQL.
- **Asisten AI** (opsional, dengan kunci API Anda sendiri: Anthropic, OpenAI, atau layanan yang
  kompatibel) di panel samping: mengenal skema, menulis dan menjelaskan SQL, menyimpan percakapannya,
  dan, jika diizinkan, menjalankan query read-only sendiri.
- Antarmuka modern yang tenang, sama dengan TernaryWire dan TernaryShell: bar atas berisi koneksi ›
  skema yang aktif dan kotak pencarian untuk setiap perintah, objek, dan script (`⇧⌘P`), rail ikon
  (navigator, project, riwayat, task), dan status bar dengan mode transaksi. Antarmuka berbahasa
  Inggris, tema terang dan gelap (View → Theme atau tombol matahari / bulan di bar atas), pintasan
  yang bisa diubah, tanpa telemetri.
- **Pembaruan otomatis** dari rilis GitHub (opsional), pasang dengan satu klik, dan catatan rilis setelah setiap pembaruan.

<p>
  <img src="docs/screenshots/data.png" width="49%" alt="Data tabel: grid dengan filter dan ORDER BY di server">
  <img src="docs/screenshots/er-diagram.png" width="49%" alt="Diagram ER database contoh">
</p>

## Instalasi

Setiap [rilis](https://github.com/ilhamnadhif/ternarygrid/releases/latest) dibuat untuk macOS, Windows, dan
Linux:

| Sistem | Unduhan |
| --- | --- |
| macOS 12 atau lebih baru (Apple Silicon, Intel) | `TernaryGrid-<versi>.dmg`, atau [installer satu baris](#macos) / [Homebrew](#homebrew) |
| Windows 10 / 11 (x64) | `TernaryGrid-<versi>-windows-x64-setup.exe` (installer) atau `-windows-x64.zip` (portable) |
| Ubuntu 22.04+, Debian 12+, Linux Mint, Pop!_OS | `TernaryGrid-<versi>-linux-amd64.deb` (`arm64` untuk ARM) |
| Fedora, RHEL / Rocky / Alma 9+, openSUSE | `TernaryGrid-<versi>-linux-x86_64.rpm` (`aarch64` untuk ARM) |
| Arch Linux, Manjaro, EndeavourOS | `TernaryGrid-<versi>-linux-x86_64.pkg.tar.zst` (`aarch64` untuk ARM) |
| Linux lainnya | `TernaryGrid-<versi>-linux-x86_64.AppImage` atau `.tar.gz` (`aarch64` untuk ARM) |

### macOS

**Installer satu baris** (disarankan):

```sh
curl -fsSL https://raw.githubusercontent.com/ilhamnadhif/ternarygrid/master/scripts/install.sh | bash
```

Script ini mengunduh rilis terbaru, memeriksanya (SHA-256 yang diterbitkan GitHub, tanda tangan kode,
bundle id dan versi), memasang `TernaryGrid.app` ke `/Applications`, lalu membukanya. Jalankan lagi untuk
memperbarui. Untuk memasang versi tertentu, berikan versinya: `… | bash -s -- v0.6.1`.

**Disk image**: unduh **`TernaryGrid-<versi>.dmg`**, buka, lalu seret **TernaryGrid** ke **Applications**.

> **“TernaryGrid” tidak bisa dibuka / “Apple could not verify…”?** Build rilis belum dinotarisasi Apple,
> jadi macOS memblokir aplikasi yang diunduh lewat browser. Pilih salah satu:
>
> - **Pakai installer satu baris** di atas. File yang diunduh dengan `curl` tidak diblokir.
> - **Izinkan sekali di Settings**: klik **Done** pada peringatan, buka **System Settings → Privacy &
>   Security**, lalu klik **Open Anyway** di samping “TernaryGrid was blocked…”.
> - **Lewat Terminal**: `xattr -dr com.apple.quarantine /Applications/TernaryGrid.app`

### Homebrew

Dari clone repositori ini (cask-nya mengikuti setiap rilis):

```sh
brew install --cask ./packaging/homebrew/ternarygrid.rb
```

### Windows

Jalankan **`TernaryGrid-<versi>-windows-x64-setup.exe`**. Installer memasang TernaryGrid untuk user Anda (tanpa
hak administrator; pilih “all users” di dialog pertamanya untuk memasang bagi semua user),
menambahkannya ke Start menu, dan bisa dihapus di **Settings → Apps**. Untuk memperbarui, jalankan
installer versi baru.

Untuk salinan **portable**, ekstrak `TernaryGrid-<versi>-windows-x64.zip` lalu jalankan `ternarygrid.exe`
(dengan `--portable <FOLDER>` agar pengaturan dan datanya disimpan di folder itu).

> **“Windows protected your PC”?** Build belum ditandatangani (code-signed), jadi SmartScreen
> memperingatkan unduhan baru: klik **More info → Run anyway**.

### Linux

Unduh paket untuk distro Anda lalu pasang:

```sh
# Ubuntu, Debian, Linux Mint, Pop!_OS
sudo apt install ./TernaryGrid-<versi>-linux-amd64.deb
# Fedora, RHEL / Rocky / Alma
sudo dnf install ./TernaryGrid-<versi>-linux-x86_64.rpm
# openSUSE
sudo zypper install --allow-unsigned-rpm ./TernaryGrid-<versi>-linux-x86_64.rpm
# Arch Linux, Manjaro, EndeavourOS
sudo pacman -U ./TernaryGrid-<versi>-linux-x86_64.pkg.tar.zst
```

Di distro lain, pakai **AppImage** (`chmod +x TernaryGrid-*.AppImage`, lalu jalankan) atau ekstrak
**tar.gz** ke `~/.local` (programnya, entri menu, dan ikonnya):
`tar -xzf TernaryGrid-<versi>-linux-x86_64.tar.gz -C ~/.local --strip-components=1`.

Setelah itu TernaryGrid ada di menu aplikasi (atau jalankan `ternarygrid`). Dibutuhkan glibc 2.34 atau lebih baru
(Ubuntu 22.04, Debian 12, Fedora 35, RHEL 9 dan yang lebih baru), X11 atau Wayland dengan OpenGL.
Kata sandi disimpan di keyring desktop lewat Secret Service (GNOME Keyring, KWallet) dan dialog file
memakai XDG desktop portal — GNOME, KDE, dan kebanyakan desktop punya keduanya; tanpa keyring, TernaryGrid
menyimpan kata sandi di brankasnya dengan kata sandi master. Untuk memperbarui, pasang paket versi
baru.

### Dari source

```sh
cargo build --release -p ternarygrid-app                     # target/release/ternarygrid (ternarygrid.exe)
scripts/bundle-macos.sh                                # macOS: dist/TernaryGrid.app, TernaryGrid-<versi>.dmg dan .zip
scripts/bundle-linux.sh                                # Linux: .deb, .rpm, paket Arch, AppImage dan .tar.gz
pwsh scripts/bundle-windows.ps1                        # Windows: installer dan .zip (perlu Inno Setup 6)
```

## Mulai

1. Klik **New connection** (atau **Import connections from DBeaver**) di halaman awal — atau
   **Create sample database** untuk mencoba semua fitur dengan database toko SQLite kecil.
2. Buka editor SQL dengan `⌘]`, ketik query, tekan `⌘↩`.
3. Klik dua kali tabel di navigator untuk mengedit datanya; klik header kolom untuk mengurutkan
   (naik → turun → mati); `⌘S` menyimpan perubahan.

Tema diatur di **Settings → General**.

## Pembaruan

Jika Anda mengizinkan, TernaryGrid mencari rilis baru setiap kali dibuka dan setiap 6 jam. Versi baru
membuka dialog berisi versi saat ini, versi baru, dan apa saja yang baru (jika ditemukan belakangan,
muncul tombol **Update** di bar atas tanpa mengganggu pekerjaan Anda). Di Windows, **Download**
mengunduh installer baru (jalankan di atas versi yang terpasang); di Linux membuka halaman rilis
berisi paket-paketnya. Di macOS, **Install update**:

1. mengunduh versi baru dan memverifikasinya (ukuran, SHA-256 yang diterbitkan GitHub, bundle id,
   versi dan tanda tangan kode);
2. setelah **Restart now**, menutup TernaryGrid (skrip yang belum disimpan ditanyakan dulu), memasang
   aplikasi baru, lalu membukanya lagi; salinan sebelumnya dikembalikan jika ada yang gagal.

macOS meminta kata sandi administrator hanya jika user Anda tidak bisa mengganti aplikasinya.
Help → Check for updates… langsung memeriksa; **Skip this version** dan **Remind me later** juga
tersedia. Setelah diperbarui, TernaryGrid menampilkan apa saja yang berubah (juga di Help → What's new;
semua catatan ada di [CHANGELOG.md](CHANGELOG.md)). Installer satu baris di atas juga bisa
memperbarui, dan pengguna Homebrew memperbarui dengan `brew upgrade --cask ternarygrid`. Pemeriksaan
pembaruan hanya satu permintaan anonim ke GitHub; tidak ada data tentang Anda atau database Anda yang
dikirim.

Pembaruan langsung memerlukan TernaryGrid di `/Applications` (atau folder lain yang bisa ditulisi); versi
0.3.0 adalah versi pertama dengan updater.

## Baris perintah

```text
ternarygrid [FILE.sql | DATABASE.db ...]   buka skrip dan file SQLite
ternarygrid --execute FILE.db FILE.sql     buka lalu jalankan skrip di database itu
ternarygrid --run-task <id|nama>           jalankan task tersimpan tanpa jendela (exit code 0/1/2/3)
ternarygrid --portable <DIR>               simpan semua pengaturan dan data di DIR
ternarygrid --bench all                    benchmark startup, memori, grid, dan editor
```

## Pintasan keyboard

| Aksi | Pintasan |
| --- | --- |
| Eksekusi statement / pilihan | `⌘↩` |
| Eksekusi skrip | `⌥X` |
| Eksekusi di tab hasil baru | `⇧⌘↩` |
| Explain plan | `⇧⌘E` |
| Batalkan query | `⌘.` |
| Editor SQL baru | `⌘]` |
| Koneksi / skema aktif | `⌘9` / `⌘0` |
| Format SQL | `⇧⌘F` |
| Autocomplete | `⌃Space` |
| Palet perintah | `⇧⌘P` |
| Simpan skrip / data | `⌘S` |

Di Windows dan Linux, `⌘` adalah `Ctrl`, `⌥` adalah `Alt`, dan `⌃` juga `Ctrl`. Semua pintasan bisa
diubah di **Settings → Keyboard shortcuts** (preset TernaryGrid atau DBeaver).

## Performa

Diukur pada build rilis (Apple M1 Pro) dengan `ternarygrid --bench all`:

| | Target | Hasil |
| --- | --- | --- |
| Ukuran biner (macOS) | < 50 MB | 49,6 MB |
| Waktu buka sampai frame pertama | < 1 dtk | ≈ 0,12 dtk |
| Memori saat diam | < 120 MB | ≈ 88 MB |
| 3 koneksi + 5 tab hasil | < 250 MB | ≈ 94 MB |
| Scroll 1 juta baris | < 16 ms / frame | p95 0,5 ms |
| Mengetik di skrip 50.000 baris | < 16 ms / frame | p95 0,5 ms |

## Keamanan dan data

- Tanpa telemetri. TernaryGrid hanya terhubung ke server yang Anda atur dan, jika diaktifkan, ke penyedia
  AI Anda serta ke GitHub untuk pemeriksaan pembaruan (satu permintaan anonim untuk rilis terbaru
  saat TernaryGrid dibuka dan setiap 6 jam; ditanyakan sekali, bisa diubah di Settings → General).
- Kata sandi dan kunci API disimpan di penyimpanan kredensial sistem (macOS Keychain, Windows
  Credential Manager, Secret Service di Linux) atau `vault.bin` (Argon2id + XChaCha20-Poly1305);
  file konfigurasi ditulis dengan mode 0600.
- Pengaturan, koneksi, skrip, riwayat, dan task ada di
  `~/Library/Application Support/dev.ternarygrid.TernaryGrid/` di macOS, `%APPDATA%\ternarygrid\TernaryGrid\` di Windows, dan
  `~/.config/ternarygrid/` + `~/.local/share/ternarygrid/` di Linux (atau `TERNARYGRID_HOME` / `--portable`).

## Build dan pengujian

```sh
cargo test --workspace
# uji integrasi terhadap server sungguhan (Docker):
export TERNARYGRID_TEST_PG=postgres://ternarygrid:ternarygrid@127.0.0.1:55432/ternarygrid
export TERNARYGRID_TEST_MYSQL=mysql://root:ternarygrid@127.0.0.1:53306/ternarygrid
export TERNARYGRID_TEST_MARIADB=mariadb://root:ternarygrid@127.0.0.1:53307/ternarygrid
export TERNARYGRID_TEST_COCKROACH='cockroachdb://root@127.0.0.1:26257/defaultdb?sslmode=disable'
export TERNARYGRID_TEST_ORACLE=oracle://ternarygrid:ternarygrid@127.0.0.1:51521/FREEPDB1   # gvenzl/oracle-free
export TERNARYGRID_TEST_CASSANDRA=cassandra://127.0.0.1:59042                  # scylladb/scylla
export TERNARYGRID_TEST_REDIS=redis://127.0.0.1:56379
export TERNARYGRID_TEST_MONGO=mongodb://127.0.0.1:57017
export TERNARYGRID_TEST_ELASTIC=http://127.0.0.1:59200                          # tanpa security
cargo test -p ternarygrid-app -p ternarygrid-driver-oracle -p ternarygrid-driver-cassandra -p ternarygrid-driver-redis \
  -p ternarygrid-driver-mongodb -p ternarygrid-driver-elasticsearch
```

Tanda tangan dan notarisasi: atur `TERNARYGRID_SIGN_IDENTITY` (Developer ID) dan `TERNARYGRID_NOTARY_PROFILE`
sebelum menjalankan `scripts/bundle-macos.sh`.

Struktur proyek ada di [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), matriks paritas DBeaver di
[docs/PARITY.md](docs/PARITY.md).

## Keterbatasan

- Hanya database yang punya driver dalam Rust yang didukung: Db2, Teradata, Informix, Snowflake,
  BigQuery, dan DuckDB tidak, begitu juga Neo4j (driver Rust-nya membawa pustaka kripto TLS kedua).
  Oracle memakai driver Rust buatan Oracle sendiri, yang masih beta.
- Hasil Redis, MongoDB, dan Elasticsearch hanya-baca di grid: ubah data dengan perintah.
- Kerberos dan autentikasi multi-faktor MySQL belum didukung.
- Build rilis belum dinotarisasi (macOS) atau ditandatangani (Windows).

## Lisensi

MIT OR Apache-2.0.

Font Inter yang disertakan (`crates/ternarygrid-app/assets/fonts`) berlisensi SIL Open Font License 1.1 (`Inter-LICENSE.txt`).
