cmake_minimum_required(VERSION 3.24)
project(raw_timelapse_desktop VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
find_package(Qt6 6.8 REQUIRED COMPONENTS Quick QuickControls2 QuickDialogs2)
find_program(CARGO_EXECUTABLE cargo REQUIRED)
qt_standard_project_setup(REQUIRES 6.8)

set(ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
set(RUST_TARGET "${CMAKE_CURRENT_BINARY_DIR}/rust")
add_custom_target(rawtl_pipeline
    COMMAND "${CARGO_EXECUTABLE}" build --release --bins
            --manifest-path "${ROOT}/pipeline/Cargo.toml" --target-dir "${RUST_TARGET}"
    VERBATIM)

qt_add_executable(raw-timelapse WIN32 MACOSX_BUNDLE
    main.cpp
    controller.cpp
    controller.h
)
set_target_properties(raw-timelapse PROPERTIES
    MACOSX_BUNDLE_BUNDLE_NAME "RAW Timelapse"
    MACOSX_BUNDLE_GUI_IDENTIFIER "com.ibobbyts.raw-timelapse"
)
add_dependencies(raw-timelapse rawtl_pipeline)
target_link_libraries(raw-timelapse PRIVATE Qt6::Quick Qt6::QuickControls2 Qt6::QuickDialogs2)
target_compile_definitions(raw-timelapse PRIVATE
    RAWTL_CLI_FALLBACK="${RUST_TARGET}/release/rawtl-linear-video${CMAKE_EXECUTABLE_SUFFIX}"
    RAWTL_WORKER_FALLBACK="${RUST_TARGET}/release/rawtl-desktop-worker${CMAKE_EXECUTABLE_SUFFIX}"
    RAWTL_NATIVE_FALLBACK="${ROOT}/native/build/dng-linear-frame${CMAKE_EXECUTABLE_SUFFIX}"
)
set_source_files_properties(qml/folder.png PROPERTIES QT_RESOURCE_ALIAS folder.png)
set_source_files_properties(qml/Main.qml PROPERTIES QT_RESOURCE_ALIAS Main.qml)
qt_add_qml_module(raw-timelapse
    URI RawTimelapse
    VERSION 1.0
    QML_FILES qml/Main.qml
    RESOURCES qml/folder.png
)

install(TARGETS raw-timelapse BUNDLE DESTINATION . RUNTIME DESTINATION bin)
if(APPLE)
    set(HELPER_DESTINATION raw-timelapse.app/Contents/MacOS)
else()
    set(HELPER_DESTINATION bin)
endif()
install(PROGRAMS
    "${RUST_TARGET}/release/rawtl-linear-video${CMAKE_EXECUTABLE_SUFFIX}"
    "${RUST_TARGET}/release/rawtl-desktop-worker${CMAKE_EXECUTABLE_SUFFIX}"
    DESTINATION "${HELPER_DESTINATION}"
)
qt_generate_deploy_qml_app_script(TARGET raw-timelapse OUTPUT_SCRIPT deploy_script)
install(SCRIPT "${deploy_script}")
