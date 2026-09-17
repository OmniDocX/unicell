# 字体加载

[项目概述](README.zh-CN.md) · [功能与限制](docs/FEATURES.md)

公开本机版默认使用浏览器可用的系统字体，未附带商业字体，也不依赖相邻产品的字体目录。

使用者可将有权使用的字体库放入 `web/fonts/`，或通过 `UNICELL_FONT_ROOT` 指向含 `manifest.json` 的目录。字体文件的使用权与分发权须由使用者自行取得。

默认清单为空，编辑器仍可运行；字体替换可能导致文字宽度、换行和打印分页发生变化。清单格式示例：

```json
{"schemaVersion":1,"files":[],"faces":[]}
```

实际字体清单中的 `files` 项须包含相对路径、SHA-256 与 MD5，`faces` 项用于关联字体族与文件。完整规则由 `web/font-runtime.js` 的 `normalizeManifest` 定义。服务对清单不使用长期缓存，对内容哈希匹配的字体文件可使用不可变缓存。通过浏览器本地字体接口授权的字体仅在本机使用。
