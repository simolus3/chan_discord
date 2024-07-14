## chan\_discord

This loadable Asterisk module adds support for joining [Discord](https://discord.com)
voice channels from Asterisk.
An active voice call to Discord is represented as an Asterisk channel, allowing them to
be bridged with other VoIP technologies supported by Asterisk.

### Installation

You can grab a prebuilt version for your Asterisk install on the GitHub releases page.
Alternatively, `chan_discord` can be built from source:

```
ASTERISK_SRC=/path/to/your/asterisk/sources cargo build --release
```

This will generate the compiled module in `target/release/libchan_discord.so`.
After obtaining a `libchan_discord.so`, put it into the modules folder of your Asterisk
install, typically `/usr/lib/asterisk/modules`.

> [!IMPORTANT]
> Discord uses Opus to encode voice data, a codec that is not supported by Asterisk
> out-of-the box. As no Opus module is openly available, this module uses its
> own encoder. __This depends on `libopus` being available on your system.__

### Setup

To join voice channels, you need your own Discord bot that will be controlled by Asterisk.
You can create one at https://discord.com/developers.
This bot needs to be added to the servers for which you're interested in joining voice channels.
To add the bot to servers, select the "bot" option under the "OAuth2" section in the Discord
developer portal and enable the "Connect" and "Speak" options under "Voice permissions".
The generated URL can be used to add the bot to your server.

Under the "Bot" page of the developer portal, you can generate a token used to authenticate
Asterisk when talking to Discord.
In your Asterisk configuration directory (usually `/etc/asterisk.conf`), create a file
named `discord.conf` and add the following content:

```
[general]
token=<your discord token>
```

### Usage

After installing the module and adding the necessary configuration options, you can restart
Asterisk.
This will add the `Discord` channel technology. To issue calls to Discord, format them as
`Discord/<server id>/<channel id>`. You can obtain the server (guild) and channel IDs by right-clicking
them in Discord.

For instance, you can forward incoming calls on an extension to Discord with:

```
same = n,Dial(Discord/1234serverid5678/1234channel5678)
```

Be aware that a bot can only be active in a single channel per server at the same time.
You also can't open multiple Asterisk channels to the same Discord call. Instead, use
a bridge to connect multiple other channels with a Discord voice chat.
