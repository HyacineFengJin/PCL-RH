# pcl-auth

Rust library used by the desktop launcher for Microsoft device authorization and Minecraft Java authentication.

## API

- `MicrosoftClient::login_device`: starts device authorization, reports the user code and progress, and accepts a cancellation flag.
- `MicrosoftClient::refresh`: renews credentials and checks that the account identity remains unchanged.
- `Session::public`: returns the account's display information and active skin/cape references.
- `Session::launch_identity`: provides the authenticated identity for the launch core.
- `save_session`, `load_session`, `delete_session`: store credentials through the operating system keyring (Linux Secret Service, Windows Credential Manager or macOS Keychain). Saving verifies that the stored bytes can be read back.

The application supplies its client ID when constructing `MicrosoftClient`. Application registration and service access are managed by the launcher maintainer.

## Credential handling

`Session` and `LaunchIdentity` do not implement `Debug` or `Serialize`. `AccountSummary` and `DeviceChallenge` expose display data for the UI. Raw authentication server error descriptions are not returned to the UI.

Credential storage returns an error if the system keyring is unavailable or locked. The caller can retain a successfully authenticated session in memory and report that it will not persist across restarts. No plaintext credential fallback is provided.
