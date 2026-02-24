# Makefile for tsnet-sidecar cross-compilation

BINARY_NAME=tsnet-sidecar
VERSION?=0.1.0
BIN_DIR=bin
GO=go

# Build flags
LDFLAGS=-ldflags "-s -w -X main.Version=$(VERSION)"

# Platforms
PLATFORMS=darwin-amd64 darwin-arm64 linux-amd64 linux-arm64 windows-amd64

.PHONY: all clean build build-all test deps

all: build

# Install dependencies
deps:
	$(GO) mod download
	$(GO) mod tidy

# Build for current platform (output to bin/ for dev)
build: deps
	mkdir -p $(BIN_DIR)
	$(GO) build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME) .

# Build for all platforms (for electron-builder packaging)
build-all: deps darwin-amd64 darwin-arm64 linux-amd64 linux-arm64 windows-amd64

# macOS Intel
darwin-amd64:
	mkdir -p $(BIN_DIR)/darwin/x64
	GOOS=darwin GOARCH=amd64 $(GO) build $(LDFLAGS) -o $(BIN_DIR)/darwin/x64/$(BINARY_NAME) .

# macOS Apple Silicon
darwin-arm64:
	mkdir -p $(BIN_DIR)/darwin/arm64
	GOOS=darwin GOARCH=arm64 $(GO) build $(LDFLAGS) -o $(BIN_DIR)/darwin/arm64/$(BINARY_NAME) .

# Linux AMD64
linux-amd64:
	mkdir -p $(BIN_DIR)/linux/x64
	GOOS=linux GOARCH=amd64 $(GO) build $(LDFLAGS) -o $(BIN_DIR)/linux/x64/$(BINARY_NAME) .

# Linux ARM64
linux-arm64:
	mkdir -p $(BIN_DIR)/linux/arm64
	GOOS=linux GOARCH=arm64 $(GO) build $(LDFLAGS) -o $(BIN_DIR)/linux/arm64/$(BINARY_NAME) .

# Windows AMD64
windows-amd64:
	mkdir -p $(BIN_DIR)/win32/x64
	GOOS=windows GOARCH=amd64 $(GO) build $(LDFLAGS) -o $(BIN_DIR)/win32/x64/$(BINARY_NAME).exe .

# Run tests
test:
	$(GO) test -v ./...

# Clean build artifacts
clean:
	rm -rf $(BIN_DIR)
	$(GO) clean

# Install locally
install: build
	cp $(BIN_DIR)/$(BINARY_NAME) /usr/local/bin/

# Development: build and run
dev: build
	./$(BIN_DIR)/$(BINARY_NAME)

# Show help
help:
	@echo "Available targets:"
	@echo "  all          - Build for current platform (default)"
	@echo "  build        - Build for current platform"
	@echo "  build-all    - Build for all platforms (for packaging)"
	@echo "  deps         - Download and tidy dependencies"
	@echo "  test         - Run tests"
	@echo "  clean        - Remove build artifacts"
	@echo "  install      - Install binary to /usr/local/bin"
	@echo "  dev          - Build and run locally"
	@echo ""
	@echo "Platform-specific builds (output to bin/\$os/\$arch/):"
	@echo "  darwin-amd64   - macOS Intel      -> bin/darwin/x64/"
	@echo "  darwin-arm64   - macOS Apple Silicon -> bin/darwin/arm64/"
	@echo "  linux-amd64    - Linux AMD64      -> bin/linux/x64/"
	@echo "  linux-arm64    - Linux ARM64      -> bin/linux/arm64/"
	@echo "  windows-amd64  - Windows AMD64    -> bin/win32/x64/"
