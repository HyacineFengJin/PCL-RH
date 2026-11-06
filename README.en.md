<p align="center">
  <img src="assets/pcl-rh.png" alt="PCL RH icon" width="112" />
</p>

# PCL RH

**Launch Minecraft, manage your games, and create your own mods.**

PCL RH is a Minecraft: Java Edition launcher built with **Rust, Tauri, and React**. It draws on the PCL family and extends game management with the RH Code workspace and a shared plugin system.

[Download latest](https://github.com/HyacineFengJin/PCL-RH/releases/latest) · [简体中文](README.md) · [User guide](docs/USAGE.md) · [Documentation](docs/README.md)

## Features

- **Launch and manage games**: install Minecraft and mod loaders, import modpacks, and manage isolated instances, mods and resources.
- **Download and maintain**: parallel downloads, automatic Java selection, dependency repair, backups, game logs and preliminary crash analysis.
- **RH Code**: an AI workspace for mod projects with multiple conversations, source editing, change review, project builds and pixel asset tools.
- **Plugins**: shared management for optional built-in capabilities and permission-controlled community components.

## Download and use

Choose your platform from [GitHub Releases](https://github.com/HyacineFengJin/PCL-RH/releases/latest):

| System | Recommended package | Support |
| --- | --- | --- |
| Linux x64 | Ubuntu 24.04 `.deb` or portable `.tar.gz` | Primary |
| Windows x64 | `*-setup.exe` or portable `.zip` | Experimental |
| macOS Apple Silicon / Intel | Matching `*-app.dmg` | Experimental |

Game acceptance on physical Windows/macOS machines is unfinished, and some features remain limited. See [platform support](docs/PLATFORMS.md). Packages are unsigned; macOS packages are not notarized by Apple.

[Install and update](docs/BUILDING.md) · [Games and modpacks](docs/USAGE.md) · [RH Code](docs/RH_CODE.md) · [Plugins](docs/PLUGINS.md) · [Report an issue](https://github.com/HyacineFengJin/PCL-RH/issues/new/choose)

For source builds, see the [build instructions](docs/BUILDING.md#源码构建). Code navigation and contribution steps are in the [contribution guide](CONTRIBUTING.md). Most detailed guides are currently in Chinese.

## License and acknowledgments

Original code and documentation are **[MIT licensed](LICENSE)**. Third-party content retains its own terms; see [copyright and provenance](COPYRIGHT.md).

Special thanks to **[龙腾猫跃 (LTCatt)](https://github.com/LTCatt)**, creator of [Plain Craft Launcher (PCL / PCL2)](https://github.com/Meloong-Git/PCL), and to [PCL CE](https://github.com/PCL-Community/PCL-CE), HMCL and other reference projects. [Support the PCL author](https://meloong.com/afd/a/LTCat). PCL's [original licensing notice and usage guide](LICENSES/PCL-LICENCE.txt) are preserved separately.

PCL RH is a third-party Rust and WebUI implementation drawing on PCL / PCL CE's design, not an official release by LTCatt or the PCL CE team, or an official Minecraft product. Project icons and primary artwork were made with **GPT**.
