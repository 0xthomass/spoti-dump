# spoti-dump

`spoti-dump` is a small command-line app that backs up, restores, or wipes the saved tracks and playlists attached to a Spotify account. It requests one combined Spotify permission grant so the same refresh token can be reused across export, import, and purge.

## Quick Start (no coding required)

1. **Create a Spotify developer app**
   1. Go to the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) and sign in.
   2. Click **Create app** → fill in any name/description → check the required boxes → Create.
   3. Open the app’s **Settings**, scroll to **Redirect URIs**, and add **`http://127.0.0.1:8000/callback`** exactly. Click **Save**.
   4. Copy the app’s **Client ID** and **Client Secret**. Keep this tab open; you’ll need them in the next step.

2. **Download the app**
   - Windows: grab the latest `spoti-dump.exe` from the Releases page.
   - macOS/Linux: download the corresponding binary or use the source instructions below.
   - Put the executable wherever you like (e.g., `Downloads/spoti-dump`).

3. **Configure `.env`**
   - Copy `.env.example` to `.env` (same folder as the executable).
   - Fill in the IDs you copied:

     ```env
     SPOTIFY_CLIENT_ID=your_client_id_here
     SPOTIFY_CLIENT_SECRET=your_client_secret_here
     SPOTIFY_REFRESH_TOKEN=
     ```

   - Leave `SPOTIFY_REFRESH_TOKEN` empty for the first run. After you authorize once, the tool prints your refresh token—paste it back into `.env` so you never have to log in again.

4. **Run a command**
   - Open a terminal (PowerShell, Command Prompt, or macOS/Linux Terminal) in the folder that contains `spoti-dump` and `.env`.
   - Example:

     ```sh
     spoti-dump export
     ```

   - Your browser opens to Spotify’s consent screen. Click **Agree** and you’re done. The CLI prints a refresh token—copy it into `.env`.

## Running commands

### Dry runs and `--force`

Every command starts in **dry-run** mode so you can see what would happen without changing anything. Add `--force` to actually perform the export/import/purge.

```sh
spoti-dump export --force
spoti-dump import --force
spoti-dump purge --force   # irreversible
```

### `export`

```
spoti-dump export [--force]
```

Creates a `dump` folder containing:
- `saved_tracks.csv` with all liked songs.
- One CSV per playlist (filenames are sanitized).

### `import`

```
spoti-dump import [--force]
```

Reads the CSVs from the `dump` folder and recreates them in the currently logged-in Spotify account.
- Make sure the `dump` folder sits next to the executable.
- Log into the **destination** Spotify account in your browser before running `import`.

### `purge`

```
spoti-dump purge [--force]
```

Deletes every liked song and unfollows every playlist in the authorized account. **There’s no undo.** Always run once without `--force` to confirm.

## Advanced: build from source

```sh
git clone https://github.com/0xthomass/spoti-dump.git
cd spoti-dump
cargo run --release -- export
```

You still need the `.env` file in the project root before running `cargo`.

### Automated Windows builds

If you just need a fresh `.exe`, run the **`build-windows-release`** workflow on GitHub:

1. Push your latest changes (or tag a release with `v*`).
2. In the GitHub UI, open **Actions → build-windows-release → Run workflow**.
3. Download the `spoti-dump-windows.zip` artifact. It contains `spoti-dump.exe`, `README.md`, and `.env.example`.
4. Attach that ZIP to your GitHub Release if you triggered the workflow manually. When you push a tag (`v1.2.3`, etc.), the workflow auto-attaches the ZIP to the release for you.

## Tips & troubleshooting

- **Browser didn’t open?** Copy the URL printed in the terminal and paste it manually.
- **“Invalid redirect URI” error?** Double-check the Spotify dashboard lists `http://127.0.0.1:8000/callback` and nothing else.
- **Ran out of time during authorization?** Just re-run the command; a new link/state will be generated.
- **Need to move to another PC?** Copy the `dump` folder and `.env` (with refresh token) along with the executable.
- **`403` during `import` or `purge` after previously running `export`?** Your saved refresh token may be read-only from an older version of the app. Clear `SPOTIFY_REFRESH_TOKEN` once and run `import` or `purge` again so Spotify can issue a token with the newer combined scopes.
