# Security

Raisin runs without admin rights, has no network access, and only talks to Razer USB devices. The links in its About section open in your browser; Raisin itself never connects to anything.

Two features look at the rest of your PC:

- **Automatic switching** reads the file name of the app in front (for example `cs2.exe`) to match your rules. It isn't logged, stored or sent anywhere; only the rules you add are saved, in `%APPDATA%\Raisin\settings.txt`.
- **Keyboard shortcuts** (off by default) are registered with Windows' `RegisterHotKey`, which only tells Raisin when its own three key combinations are pressed. Raisin never sees other typing.

Raisin also keeps a battery history on your PC, in `%APPDATA%\Raisin\battery.csv`: the time, level, voltage and charging state of each reading, and nothing else. It stays on your PC and is trimmed to the last 60 days.

If you find a security problem anyway, please report it privately through GitHub's [private vulnerability reporting](https://github.com/bladestech/Raisin/security/advisories/new) rather than a public issue.
