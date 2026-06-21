OS := $(shell uname)
CFLAGS := -Iinclude
LDFLAGS := -lm
ARCHS_IOS = aarch64-apple-ios aarch64-apple-ios-sim
ARCHS_ANDROID = aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
IOS_LIB_FILE = sqlanywhere_experimental.dylib
IOS_FINAL_LIB_FILE = sqlanywhere_experimental
LIB_FILE = sqlanywhere_experimental.so
XCFRAMEWORK = sqlanywhere_experimental.xcframework
HEADER = sqlanywhere.h

ifeq ($(OS),Darwin)
	CFLAGS += -framework Security -framework CoreServices
endif

.PHONY: all $(ARCHS_IOS) ios $(ARCHS_ANDROID) android

all: example

example: example.c
	$(CC) -o $@ $(CFLAGS) $< $(SQLANYWHERE_EXPERIMENTAL_PATH) $(LDFLAGS)

notify:
	osascript -e 'display notification "Build completed" with title "sqlanywhere"'
	echo "🟢 Build Completed!"

lint:
	cargo clippy --all-features --all-targets -- --no-deps -D warnings

fix:
	cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged
	cargo fmt --all

nuke:
	rm -rf ./target

ios: clean ios-build package-dylibs notify

clean:
	rm -rf generated/$(XCFRAMEWORK)

package-dylibs:
	mkdir -p generated
# Delete old files
	rm -rf generated/sqlanywhere*.xcframework
	
# Reset the .xcframeworks from the templates
	cp -r templates/sqlanywhere*.xcframework generated

# Pack device dylibs
	cp ../../target/aarch64-apple-ios/release/$(IOS_LIB_FILE) generated/$(IOS_FINAL_LIB_FILE)

	cp generated/$(IOS_FINAL_LIB_FILE) generated/sqlanywhere_experimental.xcframework/ios-arm64/sqlanywhere_experimental.framework/
	install_name_tool -id @rpath/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE) generated/sqlanywhere_experimental.xcframework/ios-arm64/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE)
	codesign -f -s - --identifier com.elyra.sqlanywhere-experimental generated/sqlanywhere_experimental.xcframework/ios-arm64/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE)
	
	mkdir -p generated/sqlanywhere_experimental.xcframework/ios-arm64/sqlanywhere_experimental.framework/Headers
	cp include/$(HEADER) generated/sqlanywhere_experimental.xcframework/ios-arm64/sqlanywhere_experimental.framework/Headers/

# Pack simulator dylibs (fat binary: arm64 + x86_64)
	lipo -create \
		../../target/aarch64-apple-ios-sim/release/$(IOS_LIB_FILE) \
		../../target/x86_64-apple-ios/release/$(IOS_LIB_FILE) \
		-output generated/$(IOS_FINAL_LIB_FILE)

	cp generated/$(IOS_FINAL_LIB_FILE) generated/sqlanywhere_experimental.xcframework/ios-arm64_x86_64-simulator/sqlanywhere_experimental.framework/
	install_name_tool -id @rpath/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE) generated/sqlanywhere_experimental.xcframework/ios-arm64_x86_64-simulator/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE)
	codesign -f -s - --identifier com.elyra.sqlanywhere-experimental generated/sqlanywhere_experimental.xcframework/ios-arm64_x86_64-simulator/sqlanywhere_experimental.framework/$(IOS_FINAL_LIB_FILE)
	
	mkdir -p generated/sqlanywhere_experimental.xcframework/ios-arm64_x86_64-simulator/sqlanywhere_experimental.framework/Headers
	cp include/$(HEADER) generated/sqlanywhere_experimental.xcframework/ios-arm64_x86_64-simulator/sqlanywhere_experimental.framework/Headers/

ios-build:
	cargo build --release --target aarch64-apple-ios 
	cargo build --release --target aarch64-apple-ios-sim
	SDKROOT=$$(xcrun --sdk iphonesimulator --show-sdk-path) \
	CFLAGS_x86_64_apple_ios="-target x86_64-apple-ios-simulator" \
	CXXFLAGS_x86_64_apple_ios="-target x86_64-apple-ios-simulator" \
	CC_x86_64_apple_ios=$$(xcrun --sdk iphonesimulator --find clang) \
	CXX_x86_64_apple_ios=$$(xcrun --sdk iphonesimulator --find clang++) \
	CMAKE_SYSTEM_NAME=iOS \
	CMAKE_OSX_SYSROOT=$$(xcrun --sdk iphonesimulator --show-sdk-path) \
	cargo build --release --target x86_64-apple-ios

android: $(ARCHS_ANDROID)
	rm -rf generated
	mkdir -p generated/jniLibs
	mkdir -p generated/jniLibs/arm64-v8a
	mkdir -p generated/jniLibs/armeabi-v7a
	mkdir -p generated/jniLibs/x86_64
	mkdir -p generated/jniLibs/x86
	mkdir -p generated/jniLibs/include

	cp ../../target/aarch64-linux-android/release/$(LIB_FILE) generated/jniLibs/arm64-v8a/$(LIB_FILE)
	cp ../../target/armv7-linux-androideabi/release/$(LIB_FILE) generated/jniLibs/armeabi-v7a/$(LIB_FILE)
	cp ../../target/x86_64-linux-android/release/$(LIB_FILE) generated/jniLibs/x86_64/$(LIB_FILE)
	cp ../../target/i686-linux-android/release/$(LIB_FILE) generated/jniLibs/x86/$(LIB_FILE)
	cp include/$(HEADER) generated/jniLibs/include/

$(ARCHS_ANDROID): %: 
	cargo ndk --target $@ --platform 31 build --release
