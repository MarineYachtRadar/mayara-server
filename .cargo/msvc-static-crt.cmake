# Make C/C++ dependencies built with CMake (zlib-ng via libz-sys) use the
# static C runtime too. cmake-rs passes /MT in the compiler flags, but CMake
# projects with policy CMP0091 set (zlib-ng) add their own /MD default, which
# wins and leaves unresolved __imp_ CRT symbols at link time.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
