# Uruchamianie testów FerroDb na innym komputerze

Instrukcja dla czystego laptopa: od zera do zielonych testów. Napisana przy domykaniu fazy 9 (gałąź `phase-9`). Na komputerze deweloperskim nie dało się dokończyć testów `ferrodb-core`, `ferrodb-ui` i `ferrodb-app`, bo zabrakło pamięci.

## 1. Wymagania sprzętowe

| Zasób | Minimum | Zalecane |
|---|---|---|
| RAM + plik wymiany (commit limit) | 32 GB łącznie | 48 GB+ |
| Wolne miejsce na dysku | 40 GB | 80 GB (katalog `target` z testami całego workspace'u ma ok. 60 GB) |
| Czas pierwszego builda | — | 60–90 min (DuckDB kompiluje się z C++) |

**Windows: plik wymiany.** Na maszynie z 40 GB RAM i plikiem wymiany 4,6 GB build testów kończył się losowymi błędami `can't find crate for …`, `found invalid metadata files for crate …` i `only metadata stub found`. To nie są błędy w kodzie. rustc nie może zmapować plików `.rlib`/`.rmeta`, bo brakuje pamięci (commit charge). Ustaw plik wymiany na co najmniej 16–32 GB:
*Ustawienia systemu → System → Informacje → Zaawansowane ustawienia systemu → Wydajność → Ustawienia → Zaawansowane → Pamięć wirtualna*.

Ile pamięci jest wolne, sprawdzisz tak (PowerShell):

```powershell
$os = Get-CimInstance Win32_OperatingSystem
"commit limit {0:N1} GB, wolne {1:N1} GB" -f ($os.TotalVirtualMemorySize/1MB), ($os.FreeVirtualMemory/1MB)
```

Przed buildem warto mieć **≥ 20 GB wolnego commitu**.

## 2. Narzędzia

### Windows

1. **Visual Studio 2022 Build Tools** (lub pełne VS) z workloadem **„Desktop development with C++”**. Musi zawierać MSVC v143 i **Windows 11 SDK**. Potrzebują ich DuckDB (C++), SQLite (C) i GPUI (DirectX, Windows SDK).
   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
2. **Rustup:** `winget install Rustlang.Rustup`. Wersję toolchaina (1.99.0 z clippy i rustfmt) rustup pobierze sam z `rust-toolchain.toml`.
3. **Git:** `winget install Git.Git`.
4. Opcjonalnie **cargo-nextest**, którego używa CI: `cargo install cargo-nextest --locked`.

Nie są potrzebne: CMake, NASM, OpenSSL ani Perl. Cały TLS stoi na rustls + `ring`.

### Linux (Ubuntu/Debian)

```sh
sudo apt-get install -y build-essential git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
./xtask/linux-deps.sh        # biblioteki GPUI: X11/Wayland, Vulkan, fontconfig…
```

### macOS

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 3. Kod źródłowy

Gałąź `phase-9` istnieje na razie tylko lokalnie. Przenieś ją na jeden z dwóch sposobów.

**A. Przez GitHub.** Na komputerze źródłowym:
```sh
git push -u origin phase-9
```
Na nowym laptopie:
```sh
git clone https://github.com/borjarus/ferrodb.git
cd ferrodb
git switch phase-9
```

**B. Bez wypychania: git bundle.** Na komputerze źródłowym:
```sh
git bundle create ferrodb-phase-9.bundle phase-9 master
```
Skopiuj plik (pendrive, dysk sieciowy) i na nowym laptopie wykonaj:
```sh
git clone -b phase-9 ferrodb-phase-9.bundle ferrodb
cd ferrodb
```

Pliki `.whl` ADBC i katalogi `target*` nie są potrzebne. Testy ich nie wymagają.

## 4. Uruchomienie testów

Wszystkie polecenia uruchamiasz w katalogu repozytorium. Pierwsze polecenie pobiera toolchain i zależności (wymaga internetu).

### Zmienne środowiskowe

PowerShell:
```powershell
$env:CARGO_INCREMENTAL = "0"   # mniejszy katalog target, mniej pamięci
```
bash:
```sh
export CARGO_INCREMENTAL=0
```

### Krok 1: lint (szybki, bez linkowania testów)

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Oczekiwany wynik: brak ostrzeżeń. Na komputerze źródłowym ten krok przeszedł.

### Krok 2: testy fazy 9, crate po crate'cie

Pojedyncze wywołania z ograniczoną równoległością (`-j 2`) najmniej obciążają pamięć. Kolejność jest ważna: najpierw lekkie crate'y, potem `ferrodb-core`, który ciągnie wszystkie drivery z DuckDB.

PowerShell:
```powershell
foreach ($p in "ferrodb-api","ferrodb-script","ferrodb-data","ferrodb-mock","ferrodb-ai","ferrodb-core","ferrodb-ui","ferrodb-app") {
  Write-Host "== $p"
  cargo test -j 2 --locked -p $p
  if ($LASTEXITCODE -ne 0) { Write-Host "!! $p FAILED"; break }
}
```

bash:
```sh
for p in ferrodb-api ferrodb-script ferrodb-data ferrodb-mock ferrodb-ai ferrodb-core ferrodb-ui ferrodb-app; do
  echo "== $p"; cargo test -j 2 --locked -p "$p" || { echo "!! $p FAILED"; break; }
done
```

Jeśli komputer ma dużo pamięci (≥ 64 GB commitu), możesz zwiększyć `-j` do liczby rdzeni.

### Oczekiwane wyniki

| Crate | Testy | Status na komputerze źródłowym |
|---|---|---|
| `ferrodb-api` | 3 | ✅ przeszło |
| `ferrodb-script` | 20 | ✅ przeszło |
| `ferrodb-data` | 54 | ✅ przeszło |
| `ferrodb-mock` | 66 | ✅ przeszło |
| `ferrodb-ai` | 45 (+1 ignored) | ✅ przeszło |
| `ferrodb-core` | ok. 53 (+2 ignored) w testach fazy 9 i wcześniejszych | ⏳ **do sprawdzenia** |
| `ferrodb-ui` | testy `ui`, `flows`, `phase7`, `phase9` i jednostkowe | ⏳ **do sprawdzenia** |
| `ferrodb-app` | — | ⏳ **do sprawdzenia** |

Testy oznaczone `ignored` wymagają sieci lub kluczy. Nie są wymagane do zaliczenia.

### Krok 3 (opcjonalnie): cały workspace jak w CI

```sh
cargo nextest run --workspace --locked --no-fail-fast   # albo: cargo test --workspace --locked
cargo test --workspace --doc --locked
```

Bez zmiennych `FERRODB_TEST_*` / `FERRODB_TCK_*` testy driverów sprawdzają tylko SQLite. Testy na prawdziwych bazach wymagają Dockera:

```sh
cargo xtask db-up     # PostgreSQL, MySQL, MariaDB, SSH w kontenerach
cargo xtask tck       # TCK driverów
cargo xtask db-down
```

### Test na żywym API Anthropic (opcjonalnie)

```powershell
$env:ANTHROPIC_API_KEY = "sk-ant-..."
cargo test -p ferrodb-ai live_anthropic -- --ignored
```

Test wysyła jedno prawdziwe zapytanie do API, więc zużywa trochę płatnych tokenów.

## 5. Rozwiązywanie problemów

| Objaw | Przyczyna | Co zrobić |
|---|---|---|
| `can't find crate for X`, `found invalid metadata files`, `only metadata stub found`, losowo różne crate'y | Brak pamięci (commit) przy równoległym buildzie | Zwiększ plik wymiany, użyj `-j 1`/`-j 2`, zamknij inne aplikacje, potem usuń puste pozostałości (poniżej) |
| Ten sam błąd po zwiększeniu pamięci | Puste pliki `.rmeta`/`.rlib` z przerwanego builda | `find target/debug/deps -maxdepth 1 -size 0 \( -name "*.rmeta" -o -name "*.rlib" \) -delete` (Git Bash) albo w ostateczności `cargo clean` (kosztuje ponowny build DuckDB) |
| `link.exe not found` / brak `windows.h` | Brak MSVC lub Windows SDK | Doinstaluj workload „Desktop development with C++” (punkt 2) |
| `fatal: detected dubious ownership` | Repo skopiowane z innego konta lub dysku | `git config --global --add safe.directory <ścieżka-repo>` |
| Build trwa bardzo długo przy `duckdb` / `libduckdb-sys` | Kompilacja C++ DuckDB (bundled) | Normalne przy pierwszym buildzie, kolejne korzystają z cache'u w `target` |
| Antywirus mocno spowalnia build (Windows) | Defender skanuje `target` | Dodaj katalog repozytorium do wykluczeń Defendera (wymaga uprawnień administratora) |

## 6. Po zakończeniu

Wynik dla trzech niesprawdzonych crate'ów (`ferrodb-core`, `ferrodb-ui`, `ferrodb-app`) dopisz do sekcji testów w [`docs/phase-9.md`](phase-9.md). Gdy wszystko jest zielone, gałąź `phase-9` jest gotowa do scalenia z `master`.
