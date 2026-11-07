# CMake toolchain hook for the CMake-built native dependencies on x86_64-pc-windows-msvc (ct2rs/CTranslate2,
# onednn-src/oneDNN). cmake-rs picks it up from CMAKE_TOOLCHAIN_FILE_x86_64-pc-windows-msvc (.cargo/config.toml).
#
# 1. Static CRT. The workspace links the static MSVC runtime (+crt-static). Projects whose cmake_minimum_required
#    predates CMP0091 (oneDNN: 3.13) get CMake's default "/MD" in CMAKE_<LANG>_FLAGS_<CONFIG>, which overrides
#    cmake-rs's "/MT" when a generator (Ninja) is set; the link then fails with unresolved __imp_* CRT symbols.
#    static-crt-rules.cmake rewrites the default flags to the release static runtime in every configuration.
set(CMAKE_USER_MAKE_RULES_OVERRIDE "${CMAKE_CURRENT_LIST_DIR}/static-crt-rules.cmake")
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")

# 2. Intel OpenMP. se-asr and se-translate enable ct2rs's openmp-runtime-intel on Windows (CTranslate2 must be
#    OpenMP-aware, or its thread pool calling the /openmp-compiled oneDNN makes decoding ~100x slower; COMP is
#    rejected together with MKL). CTranslate2's CMake then looks for libiomp5 under INTEL_ROOT. It is built as a
#    static library, so the path is only recorded; cargo links Intel OpenMP itself (the crates' build.rs provide
#    iomp5.lib from the MKL prebuild). oneDNN does not use this variable.
if(WIN32 AND NOT IOMP5_LIBRARY)
  set(IOMP5_LIBRARY "libiomp5md" CACHE FILEPATH "Intel OpenMP import library (linked by cargo)")
endif()
