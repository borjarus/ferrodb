# Faza 10 — dalsze kroki na innym komputerze

Instrukcja dokończenia fazy 10 (stabilizacja i wydanie 1.0) na drugim laptopie. Stan na 2026-11-23: wszystkie pakiety są scalone na gałęzi `phase-10`. Status i lista braków są w [`docs/phase-10.md`](phase-10.md). Przygotowanie maszyny (narzędzia, pamięć, plik wymiany) opisuje [`docs/running-tests.md`](running-tests.md), punkty 1–3. Ta instrukcja zakłada, że masz je za sobą.

Kroki są ułożone od najtańszego do najdroższego. Po każdym dopisz wynik do sekcji „Test runs” w `docs/phase-10.md`.

## 0. Kod

Gałąź `phase-10` istnieje tylko lokalnie. Na komputerze źródłowym:

```sh
git push -u origin phase-10          # wariant A: przez GitHub
git bundle create ferrodb-phase-10.bundle phase-10 master   # wariant B: plik
```

Na nowym laptopie:

```sh
git clone https://github.com/borjarus/ferrodb.git && cd ferrodb && git switch phase-10
# albo: git clone -b phase-10 ferrodb-phase-10.bundle ferrodb && cd ferrodb
```

Dodatkowe narzędzia dla fazy 10 (poza tym, co wymienia `running-tests.md`):

| Narzędzie | Do czego | Instalacja |
|---|---|---|
| Docker (Linux/macOS natywnie, Windows przez WSL) | kontenery TCK | Docker Desktop albo `docker` w WSL |
| Java 21 + Maven | most JDBC (H2, Derby, HyperSQL, Oracle, Db2…) | `winget install EclipseAdoptium.Temurin.21.JDK Apache.Maven` / `brew install temurin@21 maven` / `apt install openjdk-21-jdk maven` |
| Toolchain nightly + cargo-fuzz | fuzzing (najlepiej Linux lub WSL) | `rustup toolchain install nightly && cargo install cargo-fuzz --locked` |
| cargo-packager | instalatory | `cargo install cargo-packager --locked` |

## 1. Weryfikacja kodu (obowiązkowe)

```sh
export CARGO_INCREMENTAL=0           # PowerShell: $env:CARGO_INCREMENTAL = "0"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Testy crate po crate'cie (`-j 2` przy małej ilości pamięci):

```sh
for p in ferrodb-api ferrodb-lang ferrodb-data ferrodb-jdbc ferrodb-script ferrodb-mock \
         ferrodb-release ferrodb-driver-manager ferrodb-ai ferrodb-drivers ferrodb-core \
         ferrodb-ui ferrodb-app xtask; do
  echo "== $p"; cargo test -j 2 --locked -p "$p" || { echo "!! $p FAILED"; break; }
done
cargo test --workspace --doc --locked
```

| Crate | Oczekiwany wynik | Stan na komputerze źródłowym |
|---|---|---|
| `ferrodb-api` | 3 | ✅ |
| `ferrodb-lang` | 102 unit + proptest + `fuzz_smoke` | ✅ |
| `ferrodb-data` | 55 + 7 `fuzz_smoke` | ✅ |
| `ferrodb-jdbc` | 14 + 7 `fuzz_smoke` (`tests/bridge.rs` wymaga Javy) | ✅ bez `bridge.rs` |
| `ferrodb-release` | 15 + 5 + 1 | ✅ |
| `ferrodb-driver-manager`, `xtask`, `ferrodb-mock` (66), `ferrodb-ai` (45 + 1 ignored) | wszystkie zielone | ✅ |
| `ferrodb-drivers` | 58 (lib) | ✅ lib; testy integracyjne bez baz sprawdzają tylko SQLite |
| `ferrodb-ui` | 88 (lib 36, flows 23, phase7 6, phase9 8, phase10 10, ui 5) | ✅ |
| `ferrodb-core` | ok. 53 (+2 ignored) | ⏳ **do sprawdzenia** (przerwane, brak pamięci i czasu) |
| `ferrodb-script` | 20 | ⏳ **do sprawdzenia** (kod bez zmian w fazie 10) |
| `ferrodb-app` | build | ✅ clippy; testów nie ma |

Jeśli build `ferrodb-ui` kończy się błędem „paging file is too small”, dodaj `--config 'profile.dev.package.ferrodb-ui.debug=0'`.

## 2. Ręczne przeklikanie nowego UI (obowiązkowe, nikt tego nie zrobił)

```sh
cargo run -p ferrodb-app -- --mock     # bez baz; aktualizacje i raporty działają w pamięci
cargo run -p ferrodb-app               # prawdziwy rdzeń
cargo run -p ferrodb-app -- --version
```

Lista kontrolna:

- [ ] Settings ▸ **Updates and Privacy**: zmiana kanału Stable/Beta, „check automatically”, **Check Now** (bez skonfigurowanego serwera ma się pojawić czytelny komunikat, a nie błąd), zgoda na raporty Ask/Always/Never, View Reports, Open Reports Folder, Clear. Ustawienia mają przetrwać restart (`<config>/release.toml`).
- [ ] **Okno po awarii**: wymuś panikę (np. tymczasowe `panic!()` w akcji menu w buildzie debug) albo skopiuj raport JSON do katalogu z Open Reports Folder. Po restarcie przy zgodzie Ask ma się pojawić „FerroDb closed unexpectedly last time” z polem komentarza. Sprawdź Send (bez endpointu → Copy Report / Open Reports Folder), Don't Send i Always Send. Sprawdź, że hasła z connection stringów są zamaskowane w pliku raportu.
- [ ] Menu **Help**: Check for Updates…, Report a Problem…, Join Beta Program…, About FerroDb (wersja, wersja API 8, kanał); te same pozycje w palecie poleceń.
- [ ] **Poprawki z fazy 10**:
  - edycja wyników w PostgreSQL, MySQL i SQLite (wcześniej siatka zgłaszała „cannot be edited”);
  - edycja zapisanego źródła danych z SSH, trybem SSL, opcjami i filtrem schematów: po zapisie nic nie może zginąć, a `ssl.ca` dopisane ręcznie w `data-sources.toml` zostaje.
- [ ] Polski interfejs: nowe napisy przetłumaczone, bez ucięć.

## 3. Bazy danych lokalnie (TCK)

Pełny zestaw nie zmieści się w pamięci laptopa, więc uruchamiaj grupami i sprzątaj po każdej:

```sh
cargo xtask matrix                         # macierz 27 baz (to samo co docs/test-matrix.md)
cargo xtask db-up --group native && cargo xtask tck --group native && cargo xtask db-down --group native
cargo xtask db-up --group nosql  && cargo xtask tck --group nosql  && cargo xtask db-down --group nosql
cargo xtask tck --only embedded            # H2, Derby, HyperSQL, SQLite (wymaga Javy 21)
cargo xtask db-up --only oracle && cargo xtask tck --only oracle && cargo xtask db-down --only oracle
```

Priorytet: **Db2, Sybase ASE, Exasol i Hive** (`--only db2`, `sybase`, `exasol`, `hive`). Ich profile TCK powstały na podstawie dokumentacji i nigdy nie były uruchomione na serwerze. Db2 i Exasol potrzebują około 4 GB pamięci każdy i trybu privileged. Polecenia dla każdej bazy są w [`docs/test-matrix.md`](test-matrix.md). Błędy napraw w driverze albo w profilu w `crates/ferrodb-driver-tck`.

## 4. CI na GitHubie

Po `git push` gałęzi `phase-10`:

1. Uruchom ręcznie (workflow_dispatch):
   - **nightly** — macierz serwerów, 3 systemy, raport TCK w podsumowaniu;
   - **quality** — fuzzing, fuzz-smoke na 5000 przypadkach, bench-check.
2. Skonfiguruj sekrety kont chmurowych: Redshift, Azure SQL, Snowflake, BigQuery i DynamoDB na AWS. Lista nazw i limity budżetów są w `docs/test-matrix.md`. Bez sekretów te joby się pomijają z komunikatem.
3. Po pierwszych przebiegach skalibruj budżety w `benches/budgets.toml` na medianach z CI. Obecne są luźne, bo mierzono je na obciążonej maszynie.

## 5. Fuzzing (Linux lub WSL)

```sh
cargo xtask fuzz all 300        # każdy z 7 celów przez 5 minut (nightly + libFuzzer)
cargo xtask fuzz-smoke 20000    # ten sam harness na stable
```

Crashe trafiają do `fuzz/artifacts/<cel>/`. Każdy crash napraw w bibliotece i dodaj test regresji, tak jak w `docs/phase-10/quality.md`. Do zrobienia jest też zgłoszenie upstream błędu arrow-ipc 60.1, który przy niepoprawnych danych tnie bufory bez sprawdzenia.

## 6. Próba generalna wydania (bez prawdziwych certyfikatów)

```sh
cargo xtask release-keygen --out ~/ferrodb-release-test.key   # klucz POZA repozytorium; wypisze klucz publiczny
export FERRODB_UPDATE_PUBKEY=<klucz publiczny z poprzedniego kroku>
export FERRODB_UPDATE_URL=http://127.0.0.1:8000/manifest.json
cargo xtask package                                           # bez zmiennych podpisu kroki podpisu są pomijane z ostrzeżeniem
export FERRODB_RELEASE_KEY_FILE=~/ferrodb-release-test.key
cargo xtask release-manifest --version 1.0.0-rc.1 --channel beta --notes "test" --dir target/packages --base-url http://127.0.0.1:8000 --out target/packages/manifest.json
cd target/packages && python -m http.server 8000
```

Zainstaluj starszą wersję zbudowaną z tym samym `FERRODB_UPDATE_PUBKEY` i sprawdź Help ▸ Check for Updates… → Download → Install and Restart. Potem sprawdź odrzucenie: podmień bajt w paczce, usuń plik `.sig` albo podpisz manifest innym kluczem.

Z prawdziwymi certyfikatami (ustawiane jako zmienne środowiskowe lub sekrety w `release.yml`):

| Platforma | Zmienne |
|---|---|
| Windows (Authenticode) | `FERRODB_SIGN_CERT`, `FERRODB_SIGN_CERT_PASSWORD` lub `FERRODB_SIGN_CERT_SHA`, `FERRODB_SIGN_TIMESTAMP_URL` |
| macOS (codesign i notaryzacja) | `FERRODB_MACOS_SIGN_IDENTITY`; `FERRODB_NOTARY_APPLE_ID`, `FERRODB_NOTARY_TEAM_ID`, `FERRODB_NOTARY_PASSWORD` albo `FERRODB_NOTARY_KEY`, `FERRODB_NOTARY_KEY_ID`, `FERRODB_NOTARY_ISSUER` |
| Linux | `FERRODB_GPG_KEY` (opcjonalnie) |
| Aktualizacje | `FERRODB_RELEASE_KEY` (sekret CI), `FERRODB_UPDATE_PUBKEY`, `FERRODB_UPDATE_URL`, `FERRODB_RELEASE_CHANNEL` |
| Raporty awarii | `FERRODB_CRASH_ENDPOINT` |

Szczegóły są w [`docs/phase-10/release.md`](phase-10/release.md). Na macOS sprawdź pobranie sterownika Snowflake lub BigQuery (ADBC) i Javy Temurin przez Driver Manager: biblioteki mają przejść `codesign --verify` i się załadować.

## 7. Decyzje do podjęcia przed 1.0

| Temat | Opcje |
|---|---|
| Domyślny tryb SSL w dialogu to `prefer`, a plan wymaga weryfikacji domyślnie | zmiana na `verify-full` (jedna linia w `crates/ferrodb-ui/src/dialogs/data_source.rs`, `ssl_mode: 1` → 3) albo udokumentowany wyjątek |
| Podpowiadanie w konsoli 50k linii trwa 275–430 ms przy budżecie 50 ms | analiza przyrostowa (tylko bieżąca instrukcja) przed 1.0 albo po 1.0 |
| Manifest kanału beta | GitHub `releases/latest` pomija pre-release, więc manifest wymaga stałego URL (np. GitHub Pages) |
| Serwer raportów awarii | własny endpoint albo brak (wtedy tylko Copy Report) |
| Certyfikaty | zakup certyfikatu Authenticode (EV/OV), konto Apple Developer |
| Braki opisane w dokumentacji | SSL w ścieżce JDBC, Kerberos (Oracle, Vertica, Hive), pola hasła SSH, passphrase i `ssl.ca` w dialogu, Snowflake OAuth — naprawić w 1.0 czy zostawić na 1.x |

## 8. Zakończenie fazy

Gdy kroki 1–4 są zielone, dopisz wyniki do `docs/phase-10.md` i zdecyduj o scaleniu `phase-10` do `master`. Wcześniejsze gałęzie `phase-8` i `phase-9` są już w `master`.
