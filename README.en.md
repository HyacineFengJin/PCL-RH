<p align="center">
  <img src="assets/pcl-rh.png" alt="PCL RH icon" width="112" />
</p>

# PCL RH

**Launch Minecraft, manage your games, and create your own mods.**

PCL RH is a Minecraft: Java Edition launcher built with **Rust, Tauri, and React**. It draws on the PCL family and extends game management with the RH Code workspace and a shared plugin system.

[Download v0.2.0](https://github.com/HyacineFengJin/PCL-RH/releases/tag/v0.2.0) · [简体中文](README.md) · [User guide](docs/USAGE.md) · [Documentation](docs/README.md)

## Features

- **Launch and manage games**: install Minecraft and mod loaders, import modpacks, and manage isolated instances, mods and resources.
- **Download and maintain**: parallel downloads, automatic Java selection, dependency repair, backups, game logs and preliminary crash analysis.
- **RH Code**: an AI workspace for mod projects with multiple conversations, source editing, change review, project builds and pixel asset tools.
- **Plugins**: shared management for optional built-in capabilities and permission-controlled community components.

**Linux x64 is the primary supported platform. Windows x64 and macOS Intel / Apple Silicon support is experimental.** Native build and installation checks exist for Windows/macOS, but game acceptance on physical machines is unfinished, and some installation and RH Code features remain limited. See [platform support](docs/PLATFORMS.md). Packages are unsigned; macOS packages are not notarized by Apple.

## Get started

Choose the matching system and architecture from [GitHub Releases](https://github.com/HyacineFengJin/PCL-RH/releases/tag/v0.2.0). See the [build guide](docs/BUILDING.md) for installation, updates and dependencies. For a Linux source build, install the [prerequisites](docs/BUILDING.md#依赖) and run:

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
