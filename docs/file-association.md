# ファイル関連付けのセットアップ

アプリ本体は「起動時にコマンドライン引数で渡された画像パスを受け取る」ところまでを実装範囲としています
（実装指示書 §11）。OS 側で `.png` / `.jpg` を imageviewer に関連付ける作業は、以下の手順で手動で行ってください。
インストーラーは今回のスコープ外です。

どちらの OS でも、既に imageviewer が起動している状態で関連付けからファイルを開くと、
新しいプロセスは起動せず、既存ウィンドウの表示画像だけが切り替わります（位置・サイズは変わりません）。

---

## Manjaro KDE (Linux)

### 1. 実行ファイルを配置する

`config.toml` と `viewer.log` は実行ファイルと同じディレクトリに作られます。
書き込み可能な場所に置いてください（`/usr/bin` などは不可）。

```bash
mkdir -p ~/.local/opt/imageviewer
cp target/release/imageviewer ~/.local/opt/imageviewer/
```

### 2. `.desktop` ファイルを作る

`~/.local/share/applications/imageviewer.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=imageviewer
Comment=Minimal image viewer
Exec=/home/<ユーザー名>/.local/opt/imageviewer/imageviewer %f
Terminal=false
NoDisplay=false
Categories=Graphics;Viewer;
MimeType=image/png;image/jpeg;
StartupWMClass=imageviewer
```

- `Exec` の `%f` は「ローカルファイルを 1 つ渡す」指定です。`%F`（複数）にはしないでください。
  ビュワーは 1 枚ずつしか受け取りません。
- `StartupWMClass=imageviewer` は、アプリが設定する `app_id` と一致させてタスクバー上の
  アイコンをまとめるためのものです。

### 3. 登録して既定のアプリにする

```bash
update-desktop-database ~/.local/share/applications
xdg-mime default imageviewer.desktop image/png
xdg-mime default imageviewer.desktop image/jpeg

# 確認
xdg-mime query default image/png
```

KDE の GUI から設定する場合は
**システム設定 → アプリケーション → ファイルの関連付け** で `image/png` / `image/jpeg` を選び、
imageviewer を優先順位の先頭に移動します。

### 4. 解除する

```bash
rm ~/.local/share/applications/imageviewer.desktop
update-desktop-database ~/.local/share/applications
```

---

## Windows 11

### 1. 実行ファイルを配置する

`config.toml` と `viewer.log` は実行ファイルと同じフォルダに作られるため、
書き込み可能な場所に置いてください（`C:\Program Files` 配下は避ける）。

```
C:\Users\<ユーザー名>\Apps\imageviewer\imageviewer.exe
```

### 2. 「プログラムから開く」で設定する（推奨）

1. エクスプローラーで `.png` ファイルを右クリック
2. **プログラムから開く → 別のプログラムを選択**
3. **PC でアプリを選ぶ** から `imageviewer.exe` を指定
4. **常にこのアプリを使う** にチェック
5. `.jpg` / `.jpeg` についても同じ操作を行う

> **注意: 実行ファイルの場所を変えたとき（debug → release など）**
>
> Windows は「プログラムから開く」で選んだアプリを **ファイル名だけ**（`imageviewer.exe`）で覚えており、
> 最初に登録したパスを
> `HKEY_CURRENT_USER\Software\Classes\Applications\imageviewer.exe\shell\open\command`
> に保存したまま更新しません。別の場所の `imageviewer.exe` を選び直しても古いパスが起動されるため、
> 古いパスが消えている（`cargo clean` 後の `target\debug` など）と、関連付けから開いても何も起きません。
> 次のコマンドで登録パスを実際の配置先に書き換えてください。
>
> ```powershell
> Set-ItemProperty 'HKCU:\Software\Classes\Applications\imageviewer.exe\shell\open\command' '(default)' '"C:\Users\<ユーザー名>\Apps\imageviewer\imageviewer.exe" "%1"'
> ```

### 3. レジストリで設定する（複数台に配る場合）

`imageviewer-register.reg` として保存し、パスを実際の配置先に書き換えてから実行します。
バックスラッシュは `\\` と二重に書く必要があります。

```reg
Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\Classes\imageviewer.image]
@="Image file"

[HKEY_CURRENT_USER\Software\Classes\imageviewer.image\DefaultIcon]
@="C:\\Users\\<ユーザー名>\\Apps\\imageviewer\\imageviewer.exe,0"

[HKEY_CURRENT_USER\Software\Classes\imageviewer.image\shell\open\command]
@="\"C:\\Users\\<ユーザー名>\\Apps\\imageviewer\\imageviewer.exe\" \"%1\""

[HKEY_CURRENT_USER\Software\Classes\.png\OpenWithProgids]
"imageviewer.image"=""

[HKEY_CURRENT_USER\Software\Classes\.jpg\OpenWithProgids]
"imageviewer.image"=""

[HKEY_CURRENT_USER\Software\Classes\.jpeg\OpenWithProgids]
"imageviewer.image"=""
```

`%1` は開くファイルのパスです。引用符で囲まないと、空白を含むパスが複数引数に分割されます。

Windows 11 では、レジストリに ProgID を登録しても **既定のアプリは自動では切り替わりません**。
登録後に **設定 → アプリ → 既定のアプリ** で `.png` / `.jpg` / `.jpeg` に imageviewer を割り当てるか、
上記「プログラムから開く」の手順を 1 度だけ実行してください。

### 4. 解除する

`HKEY_CURRENT_USER\Software\Classes\imageviewer.image` と、各拡張子の `OpenWithProgids` 配下の
`imageviewer.image` の値を削除します。
