# Set up Reelix from a terminal only

For a server with no desktop and no browser, reached over SSH. Every step here is a command; the set-up
questions are asked in the terminal by `reelix setup`, the same questions the browser wizard asks.

## 1. Install and start Reelix

### With Docker (recommended)

1. Install Docker and its Compose plugin, if the server does not have them. On Debian or Ubuntu:

   ```bash
   curl -fsSL https://get.docker.com | sudo sh
   ```

   Other systems: see <https://docs.docker.com/engine/install/>.

   `sudo` asks for the password you sign in with. On most systems nothing shows as you type; on Ubuntu 26.04 and
   later, a star shows for each character.

2. Make a folder for Reelix and write its compose file:

   ```bash
   mkdir -p ~/reelix && cd ~/reelix
   cat > docker-compose.yml <<'EOF'
   services:
     reelix:
       image: ghcr.io/mvanhare/reelix:latest
       container_name: reelix
       environment:
         - PUID=1000
         - PGID=1000
         - TZ=Europe/London
       volumes:
         - ./config:/config
         - /data:/data   # one folder holding downloads and media
       ports:
         - "7700:7700"
       restart: unless-stopped
   EOF
   ```

   Change `/data` to the folder that holds your downloads and media, `TZ` to your time zone, and
   `PUID`/`PGID` to the output of `id -u` and `id -g`.

3. Start it, and check it is running:

   ```bash
   docker compose up -d
   docker exec reelix /app/reelix healthcheck && echo "Reelix is running"
   ```

### Without Docker (Linux, systemd)

```bash
curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh
systemctl status reelix --no-pager
```

## 2. Answer the set-up questions

If Reelix has only just started, `reelix setup` waits for it to answer, for up to two minutes.

### With Docker

```bash
docker exec -it reelix /app/reelix setup
```

### Without Docker

```bash
sudo -u reelix /opt/reelix/reelix setup --data-dir /var/lib/reelix
```

Add `--port` if Reelix does not use port 7700.

The wizard runs on the server itself, so it needs no setup token. It asks how to start (a new install,
restoring a Reelix backup, or migrating from the *arr apps), then one step at a time. Passwords and keys do
not show as you type them.

- **Keys:** `b` goes back a step, `?` shows help, `f` sends feedback, `q` stops. Enter takes the answer in
  brackets.
- **Screen readers:** add `--plain` (or set `REELIX_PLAIN=1`) for plain lines that scroll, with nothing
  redrawn and no boxes. A window smaller than 60 by 20 uses plain lines too.
- **Restore:** copy the backup to the server first (for example `scp reelix-backup-*.zip server:/data/`),
  give its path when asked, then restart Reelix: `docker restart reelix` or `sudo systemctl restart reelix`.
- **Feedback:** `f` on any step makes a report. **Show as a QR code** lets a phone post it as a GitHub
  issue when the server is not online yet; the whole report is saved in the data folder's `feedback`
  folder (`./config/feedback` with the compose file above).

## 3. Open Reelix

From a phone or laptop on the same network, open `http://<server address>:7700`. To find the address:

```bash
hostname -I | awk '{print $1}'
```

## Ask the questions again

Run the same command again at any time. Reelix asks you to sign in with an admin account first, then
leaves out the account and restore steps. In the browser, the same is in **Settings → Set-up Wizard**.
