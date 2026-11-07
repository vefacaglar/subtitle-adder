# Included by CMake while it initialises the default compiler flags (see static-crt-toolchain.cmake).
# Rust links the release static CRT (libcmt) in every cargo profile, so every configuration gets the release
# runtime; Debug (cmake-rs's choice for cargo's dev profile) is compiled like Release (no debug CRT, no /RTC,
# optimized native code even in debug test builds).
foreach(_lang C CXX)
  foreach(_cfg RELEASE DEBUG RELWITHDEBINFO MINSIZEREL)
    set(_var CMAKE_${_lang}_FLAGS_${_cfg}_INIT)
    if(DEFINED ${_var})
      string(REPLACE "/MDd" "/MT" ${_var} "${${_var}}")
      string(REPLACE "/MTd" "/MT" ${_var} "${${_var}}")
      string(REPLACE "/MD" "/MT" ${_var} "${${_var}}")
    endif()
  endforeach()
  if(DEFINED CMAKE_${_lang}_FLAGS_RELEASE_INIT)
    set(CMAKE_${_lang}_FLAGS_DEBUG_INIT "${CMAKE_${_lang}_FLAGS_RELEASE_INIT}")
  endif()
endforeach()
