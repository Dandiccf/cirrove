cmake_minimum_required(VERSION 3.24)

project(cirrove-dolphin LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_AUTOMOC ON)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/plugins")

find_package(Qt6 6.4 REQUIRED COMPONENTS Core Network Widgets)
find_package(KF6CoreAddons REQUIRED)
find_package(KF6I18n REQUIRED)
find_package(KF6KIO REQUIRED)

add_library(cirrove-dolphin-client STATIC
    cirroveclient.cpp
    cirroveclient.h
)
set_target_properties(cirrove-dolphin-client PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_link_libraries(cirrove-dolphin-client PUBLIC Qt6::Core Qt6::Network)
target_compile_options(cirrove-dolphin-client PRIVATE -Wall -Wextra -Wpedantic -Werror)

add_library(cirrovefileitemaction MODULE actionplugin.cpp)
set_target_properties(cirrovefileitemaction PROPERTIES PREFIX "")
target_compile_definitions(cirrovefileitemaction PRIVATE TRANSLATION_DOMAIN="cirrove")
target_link_libraries(cirrovefileitemaction PRIVATE
    cirrove-dolphin-client
    KF6::CoreAddons
    KF6::I18n
    KF6::KIOWidgets
)
target_compile_options(cirrovefileitemaction PRIVATE -Wall -Wextra -Wpedantic -Werror)

add_library(cirroveoverlayicon MODULE overlayplugin.cpp)
set_target_properties(cirroveoverlayicon PROPERTIES PREFIX "")
target_link_libraries(cirroveoverlayicon PRIVATE cirrove-dolphin-client KF6::KIOCore)
target_compile_options(cirroveoverlayicon PRIVATE -Wall -Wextra -Wpedantic -Werror)

include(GNUInstallDirs)
set(CIRROVE_QT_PLUGIN_DIR "${QT6_INSTALL_PLUGINS}" CACHE PATH
    "Qt plugin directory relative to CMAKE_INSTALL_PREFIX")
install(TARGETS cirrovefileitemaction
    LIBRARY DESTINATION "${CIRROVE_QT_PLUGIN_DIR}/kf6/kfileitemaction")
install(TARGETS cirroveoverlayicon
    LIBRARY DESTINATION "${CIRROVE_QT_PLUGIN_DIR}/kf6/overlayicon")

include(CTest)
if(BUILD_TESTING)
    add_executable(cirrove-dolphin-tests tests.cpp)
    target_link_libraries(cirrove-dolphin-tests PRIVATE cirrove-dolphin-client)
    target_compile_options(cirrove-dolphin-tests PRIVATE -Wall -Wextra -Wpedantic -Werror)
    add_test(NAME cirrove-dolphin-tests COMMAND cirrove-dolphin-tests)

    add_executable(cirrove-dolphin-plugin-tests plugintests.cpp)
    target_link_libraries(cirrove-dolphin-plugin-tests PRIVATE
        Qt6::Core KF6::CoreAddons KF6::KIOCore)
    target_compile_options(cirrove-dolphin-plugin-tests PRIVATE
        -Wall -Wextra -Wpedantic -Werror)
    add_test(NAME cirrove-dolphin-plugin-tests COMMAND cirrove-dolphin-plugin-tests
        $<TARGET_FILE:cirrovefileitemaction> $<TARGET_FILE:cirroveoverlayicon>)
endif()
