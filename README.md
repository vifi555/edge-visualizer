# edge-visualizer
Stunning visuals for apps like spotify or any that you choose

## Download
Go to [Releases](../../releases/latest), download **Edge.Visualizer.exe** and double-click it.
If Windows shows "Windows protected your PC", click **More info → Run anyway**.

The app updates itself when a new version is released (you can turn this off in its settings).

## Features
- Music bars around the edges of your screen, or a circle in the middle
- Synced lyrics at the bottom while someone is singing (from lrclib.net)
- Sits on top of your apps without blocking clicks, lives in the tray
- Settings save instantly · hotkeys · start with Windows

## Making a new version
Change `version` in `Cargo.toml` (for example `2.0.1` → `2.0.2`) and push. GitHub builds the exe and publishes the release automatically.
