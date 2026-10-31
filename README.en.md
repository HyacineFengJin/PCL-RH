<p align="center">
  <img src="assets/pcl-rh.png" alt="PCL RH icon" width="112" />
</p>

# PCL RH

**Launch Minecraft, manage your games, and create your own mods.**

PCL RH is a Minecraft: Java Edition launcher built with **Rust, Tauri, and React**. It draws on the PCL family and extends game management with the RH Code workspace and a shared plugin system.

[简体中文](README.md) · [User guide](docs/USAGE.md) · [Documentation](docs/README.md)

## Features

- **Launch and manage games**: install Minecraft and mod loaders, import modpacks, and manage isolated instances, mods and resources.
- **Download and maintain**: parallel downloads, automatic Java selection, dependency repair, backups, game logs and preliminary crash analysis.
- **RH Code**: an AI workspace for mod projects with multiple conversations, source editing, change review, project builds and pixel asset tools.
- **Plugins**: shared management for optional built-in capabilities and permission-controlled community components.

Native builds pass on **Linux, Windows, macOS Intel and Apple Silicon**. Cross-platform features and interactive validation are still in progress; see [platform support](docs/PLATFORMS.md) for the current scope.

## Get started

See the [build guide](docs/BUILDING.md) for candidate packages and native build entry points. For a Linux source build, install its prerequisites and run:

```sh
git clone https://github.com/HyacineFengJin/PCL-RH.git
cd PCL-RH
./build-native.sh
./start-native.sh
```

[Games and modpacks](docs/USAGE.md) · [RH Code](docs/RH_CODE.md) · [Plugins](docs/PLUGINS.md) · [Contribute](CONTRIBUTING.md)

Most detailed guides are currently in Chinese.

## License and acknowledgments

Original code and documentation are **[MIT licensed](LICENSE)**. Third-party content retains its own terms; see [copyright and provenance](COPYRIGHT.md).

Project icons and primary artwork are created with **GPT**. Thanks to the PCL family, HMCL and other projects for inspiration and references. PCL RH is independently developed and is not an official PCL CE release or Minecraft product.
