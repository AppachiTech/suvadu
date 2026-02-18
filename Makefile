.PHONY: dev test lint install help current-version release-patch release-minor release-major check-git-clean bump-version git-push crates-publish package package-linux package-linux-arm64

EXECUTABLE_NAME := suv
INSTALL_PATH := /usr/local/bin

help: ## Show this help message
	@echo 'Usage: make [target]'
	@echo ''
	@echo 'Targets:'
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

dev: ## Run the application in development mode
	cargo run

test: ## Run tests
	cargo test

lint: ## Run format check and clippy
	cargo fmt -- --check
	cargo clippy -- -D warnings

install: ## Build release binary and install to /usr/local/bin (may require sudo)
	cargo build --release
	sudo cp target/release/$(EXECUTABLE_NAME) $(INSTALL_PATH)/
	# Create symlink so 'suvadu' works too
	sudo ln -sf $(INSTALL_PATH)/$(EXECUTABLE_NAME) $(INSTALL_PATH)/suvadu

uninstall: ## Remove binary and symlink from /usr/local/bin (may require sudo)
	sudo rm -f $(INSTALL_PATH)/$(EXECUTABLE_NAME)
	sudo rm -f $(INSTALL_PATH)/suvadu
	# Remove shell integration hook from .zshrc
	@if [ -f "$$HOME/.zshrc" ]; then \
		echo "Removing shell integration hook from $$HOME/.zshrc"; \
		sed -i '' '/eval "$$(suv init zsh)"/d' "$$HOME/.zshrc"; \
	fi
	@for file in ".bash_profile" ".bashrc"; do \
		if [ -f "$$HOME/$$file" ]; then \
			echo "Removing shell integration hook from $$HOME/$$file"; \
			sed -i '' '/eval "$$(suv init bash)"/d' "$$HOME/$$file"; \
		fi; \
	done

current-version: ## Output current version from Cargo.toml
	@sed -n 's/^version = "\([0-9.]*\)"/\1/p' Cargo.toml | head -n1

# Internal Release Helpers
check-git-clean:
	@if [ -n "$$(git status --porcelain)" ]; then \
		echo "\033[31mError: Git working directory is dirty. Please commit or stash changes first.\033[0m"; \
		exit 1; \
	fi

git-push:
	git push origin HEAD
	git push origin v$$(make current-version)

crates-publish:
	cargo publish

bump-version:
	@current=$$(make current-version); \
	major=$$(echo $$current | cut -d. -f1); \
	minor=$$(echo $$current | cut -d. -f2); \
	patch=$$(echo $$current | cut -d. -f3); \
	if [ "$(BUMP)" = "major" ]; then \
		major=$$((major + 1)); minor=0; patch=0; \
	elif [ "$(BUMP)" = "minor" ]; then \
		minor=$$((minor + 1)); patch=0; \
	elif [ "$(BUMP)" = "patch" ]; then \
		patch=$$((patch + 1)); \
	fi; \
	new_ver="$$major.$$minor.$$patch"; \
	echo "Bumping version: $$current -> $$new_ver"; \
	sed -i '' "s/^version = \"$$current\"/version = \"$$new_ver\"/" Cargo.toml; \
	git commit -am "chore: bump version to $$new_ver"; \
	git tag "v$$new_ver"; \
	echo "\033[32mSuccessfully bumped to $$new_ver\033[0m"

# Public Release Commands
release-patch: check-git-clean ## Bump patch version (0.1.0 -> 0.1.1), push, and publish to crates.io
	@$(MAKE) bump-version BUMP=patch
	@$(MAKE) git-push
	@$(MAKE) crates-publish

release-minor: check-git-clean ## Bump minor version (0.1.x -> 0.2.0), push, and publish to crates.io
	@$(MAKE) bump-version BUMP=minor
	@$(MAKE) git-push
	@$(MAKE) crates-publish

release-major: check-git-clean ## Bump major version (0.x.x -> 1.0.0), push, and publish to crates.io
	@$(MAKE) bump-version BUMP=major
	@$(MAKE) git-push
	@$(MAKE) crates-publish

package: ## Create a macOS release tarball with binary and license
	cargo build --release
	cp target/release/$(EXECUTABLE_NAME) .
	tar -czf $(EXECUTABLE_NAME)-macos.tar.gz $(EXECUTABLE_NAME) LICENSE
	rm -f $(EXECUTABLE_NAME)
	@echo "\033[32mCreated $(EXECUTABLE_NAME)-macos.tar.gz\033[0m"

package-linux: ## Cross-compile for Linux x86_64 (requires cross-rs)
	cross build --release --target x86_64-unknown-linux-gnu
	cp target/x86_64-unknown-linux-gnu/release/$(EXECUTABLE_NAME) .
	tar -czf $(EXECUTABLE_NAME)-linux-x86_64.tar.gz $(EXECUTABLE_NAME) LICENSE
	rm $(EXECUTABLE_NAME)
	@echo "\033[32mCreated $(EXECUTABLE_NAME)-linux-x86_64.tar.gz\033[0m"

package-linux-arm64: ## Cross-compile for Linux ARM64 (requires cross-rs)
	cross build --release --target aarch64-unknown-linux-gnu
	cp target/aarch64-unknown-linux-gnu/release/$(EXECUTABLE_NAME) .
	tar -czf $(EXECUTABLE_NAME)-linux-aarch64.tar.gz $(EXECUTABLE_NAME) LICENSE
	rm $(EXECUTABLE_NAME)
	@echo "\033[32mCreated $(EXECUTABLE_NAME)-linux-aarch64.tar.gz\033[0m"
