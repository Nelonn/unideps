# UniDeps Documentation

UniDeps is a deterministic C/C++ dependency orchestrator with native CMake integration, content-addressed caching, source patching, and cross-compilation support.

---

## 1. Core Architecture

UniDeps reads `unideps.toml` (plus an optional, uncommitted `.local.toml` next to it), resolves a dependency graph, builds every package with CMake in dependency order, and writes a CMake script that imports the results.

```
<base dir>  (default: ~/.unideps)
├── sources/    <- git checkouts and patched source trees
├── scratch/    <- CMake build directories (wiped by `unideps clean`)
├── installed/  <- installed package prefixes
├── cache/      <- compressed .tar.zst packages
├── locks/      <- lock files serialising concurrent runs
└── logs/       <- configure/build/install logs per package
```

The storage directory is chosen in this order: `--base-dir` (or `unideps_setup(BASE_DIR ...)`), the `UNIDEPS_DIR` / `UNIDEPS_BASE_DIR` environment variables, `storage.base_dir` in `.local.toml`, then `~/.unideps`.

Runs that share a storage directory are serialised with a lock file, so parallel CMake configures are safe.

### Two-Level Hashing

1. **`source_id`** — what is built:
   - package name and `version`;
   - git URL and the commit / tag / branch; for branches and the default branch, the commit it currently resolves to;
   - for `path` sources, the contents of every file in the directory (except `.git`);
   - patch file names and contents.

2. **`build_id`** — how it is built:
   - `source_id`;
   - target triple (host triple for tools);
   - compiler family and a fingerprint of the compiler executables (path, size, mtime) and the toolchain file contents;
   - MSVC runtime (MSVC targets only), C++ standard, C++ standard library, LTO;
   - build type, linkage (`shared`), `header_only`;
   - `flags` and `cmake_options` (including forwarded `ANDROID_*` settings such as `ANDROID_PLATFORM` and `ANDROID_STL`);
   - `build_id`s of all transitive dependencies.

A package is installed to `installed/<name>-<build_id>` and archived to `cache/<name>-<build_id>.tar.zst`. An install counts as complete only after it finishes; interrupted builds and corrupt archives are detected and rebuilt.

Tags and commits are treated as immutable, so cache hits for them need no network access. Branch sources are fetched on every run to pick up new commits.

---

## 2. Manifest Specification (`unideps.toml`)

### `[package]`

```toml
[package]
name = "my_app"
version = "1.0.0"
```

### `[dependencies]`

Each dependency needs a source: `git`, `path`, or a `recipe` that provides one.

```toml
[dependencies.zlib]
git = "https://github.com/madler/zlib.git"
tag = "v1.3.1"
patches = ["patches/001-fix-cmake.patch"]
shared = false
auto_import = true
cmake_options = { ZLIB_BUILD_EXAMPLES = "OFF" }
```

Version-only specs (`fmt = "10.2.1"`) require registries, which are not supported yet; UniDeps reports an error for them.

#### Supported Fields

| Field | Type | Description |
|---|---|---|
| `git` | String | Git repository URL |
| `commit` | String | Commit SHA (at least 7 hex characters). Highest priority ref |
| `tag` | String | Git tag |
| `branch` | String | Git branch (fetched on every run) |
| `path` | String | Local source directory, relative to the manifest |
| `version` | String | Informational version, also passed to recipes |
| `shallow` | Boolean | Shallow fetch (default `true`) |
| `strategy` | String | Build strategy. Only `"cmake-install"` (default) is supported |
| `shared` | Boolean | Build shared (`true`) or static (`false`). Unset: `BUILD_SHARED_LIBS` of the consuming project, else static |
| `header_only` | Boolean | Skip compilation and copy `include_dirs` (default `include/`) from the source into the install prefix |
| `cmake_options` | Table | Definitions passed as `-D<KEY>=<VALUE>`. `CMAKE_C_FLAGS`/`CMAKE_CXX_FLAGS` are merged with `flags`. Values may refer to variables of the consuming CMake project, see [Variables in `cmake_options`](#variables-in-cmake_options) |
| `patches` | Array of paths | Patches (`-p1` unified diffs) applied with `git apply`, falling back to `patch` |
| `platforms` | Array of strings | Only build for matching targets. Values: `windows`, `linux`, `macos`/`darwin`, `ios`, `android`, `emscripten`, `x86_64`, `x86`, `arm`, `aarch64`/`arm64`, `wasm32`, `riscv64`, `msvc`, `gnu`, `musl`, or a full triple. Prefix `!` to exclude |
| `enabled_if` | String | Condition over CMake variables of the consuming project, e.g. `"WITH_PNG"`, `"!NO_ZLIB"`, `"BACKEND=vulkan"`, `"A && !B"`, `"A \|\| B"` |
| `abi_ignores` | Array of strings | Settings excluded from `build_id`: `compiler`, `vc_runtime`, `cxx_std`, `cxx_stdlib` |
| `import_name` | String | CMake package name for `find_package`, and an extra `<import_name>::<import_name>` alias |
| `dependencies` | Array | Names of other entries in `[dependencies]` this package needs |
| `tools` | Array of strings | Names of entries in `[tools]` needed to build this package |
| `recipe` | Path | `.toml` or `.lua` recipe filling in unset fields (see section 4) |
| `auto_import` | Boolean | Synthesise `<name>::<name>` from the installed files instead of calling `find_package` |
| `include_dirs` | Array of paths | Include directories relative to the install prefix (default `include`) |
| `libraries` | Array of strings | Libraries relative to the install prefix. Extensionless names (`"yuv"`, `"lib/yuv"`) resolve to `.lib`, `.a`, `.so` or `.dylib`. All listed libraries are linked |
| `binaries` | Array of paths | Runtime binaries (DLLs) relative to the install prefix |

Setting `include_dirs`, `libraries` or `header_only` also enables `auto_import`.

#### Import modes

- **`find_package` mode** (default): after building, UniDeps calls `find_package(<import_name or detected config name> CONFIG)` against the install prefix and reports the imported targets. Configure fails with a clear message if no config package is installed.
- **`auto_import` mode**: UniDeps creates `<name>::<name>` (and a plain `<name>` alias). It is `SHARED IMPORTED` when a DLL is found for a shared build, `INTERFACE IMPORTED` for header-only packages, and `UNKNOWN IMPORTED` otherwise. Dependencies listed in `dependencies` are linked to it.

In both modes, `<NAME>_ROOT` variables and `CMAKE_PREFIX_PATH` entries are set so that plain `find_package(<Name>)` calls in your project also work.

### `[tools]`

Host build tools (code generators, assemblers). They are built for the host with the host's default compiler, even when cross-compiling, and their `bin/` directories are put on `PATH` for the packages that list them in `tools`.

```toml
[tools.nasm]
git = "https://github.com/netwide-assembler/nasm.git"
tag = "nasm-2.16.03"
```

Tools support `git`, `tag`, `branch`, `commit`, `path`, `version` and `strategy`. A tool can be replaced by a preinstalled one in `.local.toml` (see section 3).

### `[presets]`

Named configurations selected with `--preset` or `unideps_setup(PRESET ...)`:

```toml
[presets.debug-msvc]
build_type = "Debug"
vc_runtime = "MultiThreadedDebugDLL"   # alias: cxx_runtime
cxx_std = "20"
flags = ["/W4"]

[presets.release-clang]
build_type = "Release"
cxx_std = "c++20"
cxx_stdlib = "libc++"
lto = "thin"
flags = ["-O3"]
```

| Field | Values |
|---|---|
| `build_type` | `Debug`, `Release`, `RelWithDebInfo`, `MinSizeRel` |
| `vc_runtime` | `MultiThreaded`, `MultiThreadedDebug`, `MultiThreadedDLL`, `MultiThreadedDebugDLL` (or `static`, `static_debug`, `dynamic`, `dynamic_debug`) |
| `cxx_std` | `"17"`, `"c++20"`, `"gnu++17"`… Sets `CMAKE_CXX_STANDARD` / `CMAKE_CXX_EXTENSIONS`. Unset: the package decides |
| `cxx_stdlib` | `default`, `libc++`, `libstdc++` (adds `-stdlib=...`; Clang only), `custom` |
| `lto` | `true`/`false` or `"thin"`, `"full"`, `"off"`. Enables `CMAKE_INTERPROCEDURAL_OPTIMIZATION` |
| `flags` | Extra C and C++ compiler flags |

The build type and MSVC runtime of the consuming CMake project take precedence over the preset.

### `[[strategy]]`

Rules that adjust how packages are built:

```toml
[[strategy]]
package = "openssl"
scope = "exact"
shared = false

[[strategy]]
package = "app_core"
scope = "cascade"
cxx_std = "17"

[[strategy]]
scope = "global"
build_type = "Release"
```

Fields: `build_type`, `shared`, `vc_runtime`, `cxx_std`, `cxx_stdlib`, `lto`, `flags`, `cmake_options`.

`scope`:
- `exact`: only `package`.
- `cascade`: `package` and all of its transitive dependencies.
- `global`: every package (no `package` field).

Precedence: global < cascade inherited from dependents < the package's own cascade < exact.

### `[overrides]`

Replace fields of a dependency, e.g. to pin a fork. Setting `git` or `path` replaces the whole source, and setting `commit`, `tag` or `branch` replaces all previous refs:

```toml
[overrides.openssl]
git = "https://github.com/openssl/openssl.git"
tag = "openssl-3.3.1"
```

### `[targets."<triple>"]`

Per-target toolchain defaults (the CMake integration normally passes these for you):

```toml
[targets."aarch64-linux-android"]
toolchain_file = "/opt/android-ndk/build/cmake/android.toolchain.cmake"

[targets."x86_64-unknown-linux-gnu"]
c_compiler = "clang"
cxx_compiler = "clang++"
compiler = "clang"
```

### `[registries]`

Reserved for recipe registries. Not supported yet: the section is parsed, and UniDeps prints a warning that it is ignored.

---

## 3. Local Configuration (`.local.toml`)

Machine-specific settings, placed next to `unideps.toml` and not committed:

```toml
[storage]
base_dir = "D:/unideps"        # storage directory
scratch_dir = "R:/unideps-tmp" # build directories (e.g. a RAM disk)
keep_build_dirs = false        # delete build directories after a successful build

[resources]
max_jobs = 8                   # passed to `cmake --build --parallel`

[tools]
nasm = "C:/tools/nasm"         # use a preinstalled tool (its dir or bin/ goes on PATH)

[targets."x86_64-pc-windows-msvc"]
c_compiler = "C:/Program Files/LLVM/bin/clang-cl.exe"

[overrides.zlib]
path = "../zlib"               # develop against a local checkout
```

`targets` and `overrides` here take precedence over `unideps.toml`. The `[cache]` section, `storage.scratch_mode`, `storage.cache_sources` and `resources.max_memory_gb` are reserved and produce a warning.

---

## 4. Recipes

A recipe describes how to obtain and import a package. Reference it with `recipe = "path/to/recipe.toml"` (or `.lua`). Fields set in the manifest always win over the recipe.

### TOML recipe

```toml
name = "libogg"
version = "1.3.5"
strategy = "cmake-install"
import_name = "Ogg"
patches = ["patches/cmake-fix.patch"]   # relative to the recipe file

[source]
git = "https://github.com/xiph/ogg.git"
tag = "v1.3.5"

[options]           # passed as cmake_options (true/false -> ON/OFF)
BUILD_TESTING = false
```

Top-level keys (`patches`, `dependencies`, …) must come before the first `[table]`.

### Lua recipe

Lua recipes run in a sandbox with only the `table`, `string`, `math` and `utf8` libraries; they cannot access files, the network or processes.

```lua
package = {
    name = "ogg",
    latest = "1.3.5",
    import_name = "Ogg",
    source = function(version)
        return { git = "https://github.com/xiph/ogg.git", tag = "v" .. version }
    end,
    dependencies = { "zlib" },
    options = { BUILD_TESTING = false },
}
```

`source`, `dependencies` and `tools` may be tables or functions. The requested `version` from the manifest (or `latest`) is passed to `source`.

---

## 5. CMake Integration

```cmake
cmake_minimum_required(VERSION 3.20)
project(demo CXX)

include(cmake/unideps.cmake)
unideps_setup()

add_executable(demo main.cpp)
target_link_libraries(demo PRIVATE fmt::fmt zlib::zlib)
```

### `unideps_setup()` parameters

All parameters are optional:

```cmake
unideps_setup(
    MANIFEST    "${CMAKE_CURRENT_SOURCE_DIR}/unideps.toml"
    BASE_DIR    "${CMAKE_SOURCE_DIR}/.unideps"
    PRESET      "release-clang"
    TARGET_FILE "${CMAKE_CURRENT_BINARY_DIR}/unideps_targets.cmake"
)
```

| Parameter | Default | Description |
|---|---|---|
| `MANIFEST` | `${CMAKE_CURRENT_SOURCE_DIR}/unideps.toml` | Path to the manifest |
| `BASE_DIR` | `${UNIDEPS_BASE_DIR}`, else CLI default (`~/.unideps`) | Storage directory |
| `PRESET` | `${UNIDEPS_PRESET}` | Preset name |
| `TARGET_FILE` | `${CMAKE_CURRENT_BINARY_DIR}/unideps_targets.cmake` | Generated targets script |

The `unideps` executable is looked up on `PATH` and next to the module (`../target/release`, `../target/debug`); set `UNIDEPS_EXECUTABLE` to override.

### What `unideps_setup()` passes on

- target triple (Windows MSVC/GNU, macOS, iOS, Linux, Emscripten, Android NDK);
- `CMAKE_BUILD_TYPE`, `BUILD_SHARED_LIBS`, `CMAKE_MSVC_RUNTIME_LIBRARY` (generator expressions like `MultiThreaded$<$<CONFIG:Debug>:Debug>DLL` are expanded);
- C/C++ compilers, compiler family and `CMAKE_TOOLCHAIN_FILE`;
- `ANDROID_ABI`, `ANDROID_PLATFORM`, `ANDROID_NDK`, `ANDROID_STL`, `CMAKE_ANDROID_ARCH_ABI`;
- cache options (`option()`, `-D`) and boolean variables, for `enabled_if`.

Editing `unideps.toml` re-runs CMake configure automatically.

### Variables in `cmake_options`

`${NAME}` in a `cmake_options` value (of a dependency, an override, a `[[strategy]]` rule or a recipe) is replaced by the value of the CMake variable `NAME` of the project that calls `unideps_setup()`; `$ENV{NAME}` by an environment variable:

```toml
[dependencies.openmedia]
git = "https://github.com/Nelonn/OpenMedia"
cmake_options = { MY_SDK = "${MY_SDK}", MY_SDK_INCLUDE = "${MY_SDK}/include" }
```

```cmake
set(MY_SDK "${CMAKE_CURRENT_SOURCE_DIR}/mysdk" CACHE PATH "") # before unideps_setup()
unideps_setup()
```

- `unideps_setup()` passes on every variable that the manifest refers to this way, whatever its type (paths included); the variable has to be defined before the call. Outside CMake use `--cmake-args=-DMY_SDK=...`.
- An undefined variable is an error, not an empty string.
- The expanded value is what the dependency is built with, so it is part of the build id: another `MY_SDK` means another build.
- Only the root manifest is scanned for references. In the `unideps.toml` of a dependency `${NAME}` can use variables that the root manifest refers to as well, or `$ENV{NAME}`.
- The option reaches the dependency the entry is written for. It is not passed on to that dependency's own nested dependencies.

### Nested manifests

If the source of a dependency contains a `unideps.toml`, unideps builds the dependencies declared there first, in the same run and with the same target and compiler:

- they end up in the root `unideps_targets.cmake`, so the project can link them directly. If the project declares a package with the same name itself, the project's build is used and a warning is printed; between nested manifests the first one wins;
- their prefixes are added to `CMAKE_PREFIX_PATH` of the dependency's build and their build ids are part of the dependency's build id;
- the dependency's own `unideps_setup()` does **not** start `unideps`: the outer run generates a targets file for the nested dependencies and passes it as `UNIDEPS_NESTED_TARGETS`, which `unideps_setup()` includes. `MANIFEST`, `TARGET_FILE` and the other parameters are ignored in this mode;
- `[strategy]` rules of the nested manifest apply to its dependencies; `[presets]`, `[targets]`, `[overrides]` and `.local.toml` of the nested manifest are ignored, the outer ones are used for local tools and resource limits;
- `enabled_if` in the nested manifest refers to the options of the dependency itself: its `option()` defaults, overridden by the dependency's `cmake_options` (the options of the outer project are not used). They are only known once its CMake has run, so unideps first configures the dependency once in a scratch directory (`[PROBE]`); its `unideps_setup()` writes the options declared before it and stops the configure. The result is cached, and the probe is skipped if no nested dependency uses `enabled_if`. Declare the options **before** `unideps_setup()`, and keep the dependency's `cmake/unideps.cmake` up to date: an older copy cannot be probed and all `enabled_if` dependencies are treated as disabled (with a warning);
- cycles between nested manifests are reported as an error;
- only target dependencies are inspected, `[tools]` are not. `unideps fetch` follows nested manifests too.

The nested manifest is found in the (patched) source, so a dependency's source has to be present even when its build is cached; an existing checkout is reused without network access.

### Nesting guard

Every CMake process started by unideps has `UNIDEPS_ACTIVE=1` in its environment. A second `unideps` started from inside would wait forever for the build lock held by the first, so `unideps` refuses to run when the variable is set. `unideps_setup()` checks it too and switches to the nested mode above.

### Limitations

- Multi-config generators (Visual Studio, Ninja Multi-Config): dependencies are built once, for `CMAKE_BUILD_TYPE` or Release. Pass `-DCMAKE_BUILD_TYPE=Debug` to get debug dependencies.
- Dependencies themselves are always built with Ninja.

---

## 6. CLI Command Reference

### `unideps build`

```bash
unideps build --manifest unideps.toml --target x86_64-pc-windows-msvc --build-type Release --compiler clang
```

| Option | Value | Description |
|---|---|---|
| `-m, --manifest` | `<PATH>` | Path to `unideps.toml` (default `unideps.toml`) |
| `-t, --target` | `<TRIPLE>` | Target triple, e.g. `x86_64-pc-windows-msvc`, `x86_64-w64-mingw32`, `aarch64-linux-android` (default: host) |
| `-p, --preset` | `<NAME>` | Preset from `[presets]` |
| `--build-type` | `<TYPE>` | `Debug`, `Release`, `RelWithDebInfo`, `MinSizeRel` |
| `--default-shared` | `<BOOL>` | Linkage for dependencies without `shared` (`ON`/`OFF`, `true`/`false`, `1`/`0`) |
| `--msvc-runtime` | `<RUNTIME>` | MSVC runtime; `$<$<CONFIG:...>:...>` expressions are expanded |
| `--compiler` | `<TYPE>` | `clang`, `msvc`, `gcc` or a custom name |
| `--c-compiler` / `--cxx-compiler` | `<PATH>` | Compiler executables |
| `--toolchain-file` | `<PATH>` | CMake toolchain file for target dependencies |
| `--cmake-args` | `-D<KEY>=<VALUE>` | Options for `enabled_if`; `ANDROID_*` ones are forwarded to builds. Repeatable |
| `--generate-targets` | `<PATH>` | Output path of the CMake targets script (default `<base>/installed/unideps.cmake`) |
| `--base-dir` | `<DIR>` | Storage directory |

Invalid values (unknown build type, runtime, preset or triple) are errors, not silently ignored.

### `unideps fetch`

Downloads all git sources without building:

```bash
unideps fetch --manifest unideps.toml
```

### `unideps clean`

Removes the scratch build directories. Sources, installed packages and the cache are kept:

```bash
unideps clean
```

---

## 7. Cross-Compilation & Platform Specifics

### Android NDK

```bash
cmake -B build-android -G Ninja \
  -DCMAKE_TOOLCHAIN_FILE=$ANDROID_NDK/build/cmake/android.toolchain.cmake \
  -DANDROID_ABI=arm64-v8a \
  -DANDROID_PLATFORM=android-26
```

UniDeps:
- selects the target triple `aarch64-linux-android` (from `ANDROID_ABI`);
- builds target dependencies with the NDK toolchain and the same `ANDROID_*` settings (which are part of the cache key);
- builds `[tools]` for the host, with the host compiler;
- passes all dependency prefixes via `CMAKE_PREFIX_PATH` / `CMAKE_FIND_ROOT_PATH` and `PKG_CONFIG_PATH`.

### Windows PDB collection

For Windows targets, CMake packages often do not install `.pdb` files. After installing, UniDeps finds the PDBs referenced by the installed `.lib`/`.dll`/`.exe` files in the build tree and copies them next to those files.
