# TL Supporter

[English](#english) · [Українська](#українська)

---

## English

A local network tool for team leads: see and hear an employee, draw hints on their screen,
talk into their headset, send text and take over the mouse and keyboard when needed.

Requirements: Windows 10 version 2004 or later, Windows 11. A single file, `TLSupporter.exe`.

Status: first version for testing. This repository is private, there is no license yet and all rights are reserved.
The program is free of charge, a free license is planned for later.

### Features

- List of employees online, as a list or thumbnails, with groups for several teams.
- Switch on the fly: audio only, eco mode or full screen.
- The lead hears the employee's computer sound and microphone.
- The lead's microphone runs constantly or as push-to-talk, always starts off, and the employee hears it in both ears.
- Several leads at once, with the author's name next to each drawing.
- On-screen drawing: two markers, rectangle, arrow, laser. Drawings gradually fade away.
- Text hints, sets of ready phrases, a "Need help" button with an optional reason.
- Mouse and keyboard control that the lead turns on and off at will. The clipboard is shared only on a full connection.
- Full access on the lock screen, the sign-in screen and administrator windows, as a separate right for each lead.
- Lead rights, an action journal, console lock and auto-lock, an admin password on the employee's PC.
- Adding a PC with a one-time code, presets for quick installation, settings backup.
- Autostart as a Windows service, the employee sees only a tray icon.
- The window's close button hides the program to the tray. It closes completely only from the tray menu or settings, with the team admin password.
- A lead's PC can switch to employee mode and back with the team admin password.
- Updates: a lead's PC downloads a signed version from GitHub and distributes it over the local network.

### How it works

There is no dedicated server. Team data is stored on the PCs themselves and synchronized between them.
Each PC has its own key, so a changing IP address does not break the connection. Connections are encrypted (QUIC, TLS 1.3)
and accepted only from local network addresses. Until a lead connects, screen and audio are not sent anywhere.

Stack: Rust, egui, quinn, Opus, DXGI Desktop Duplication, WASAPI.

### Testing status

Tested with two copies on one PC: discovery by key, join code, screen, thumbnails, drawings, text hints,
control, voice, computer sound, switching to the secure channel on a simulated lock.

Not yet tested on real PCs: service installation, autostart, firewall, operation between two computers,
real lock and sign-in screens and administrator windows, drawing and control over full-screen mstsc, publishing an update.

### Installation

1. Run `TLSupporter.exe` on the lead's PC, choose "Створити нову команду" (create a new team) and confirm the administrator prompt.
2. On the lead's PC: Налаштування (Settings) → Додати ПК (Add PC) → Створити код (Create code).
3. On the employee's PC, run the same file and choose "Приєднатись за кодом" (join with a code).
4. On the lead's PC, press "Підтвердити" (Confirm) next to the new PC.

Silent installation with a preset (as administrator):

    TLSupporter.exe --install --preset agent.tlpreset --preset-password PASSWORD

Uninstall (as administrator): `TLSupporter.exe --uninstall`

Network: UDP 47700–47703, the firewall rule is added for the local subnet only.

Full access (sign-in screen, Windows lock, administrator windows) is enabled separately for each lead:
Налаштування (Settings) → Права керівників (Lead rights). It is handled by a separate process with SYSTEM rights started by the service.
Installation enables the `SoftwareSASGeneration=1` policy so that the Ctrl+Alt+Del button works.
Data is stored in `C:\ProgramData\TLSupporter`, the program log is in `tlsupporter.log`.

### Build

Requires Rust, Visual Studio Build Tools 2022 and CMake (included in Build Tools, must be added to PATH):

    cargo build --release

Output: `target\release\TLSupporter.exe`.

### Publishing an update

The private signing key is kept outside the project: `%USERPROFILE%\.tlsupporter\release_private.key`.
Updates cannot be published without it, so keep a backup.

    TLSupporter.exe --sign-release TLSupporter.exe %USERPROFILE%\.tlsupporter\release_private.key 0.2.0

Upload `TLSupporter.exe` and `TLSupporter.exe.sig` to a GitHub Release tagged `v0.2.0`.
Releases must be in a public repository, set as `owner/name` in the settings.
Add new protocol fields only in a way that older versions can ignore them (see `src/proto.rs`).

### Running two copies on one PC

The variables `TLS_DATA_DIR`, `TLS_QUIC_PORT`, `TLS_DISCOVERY_PORT`, `TLS_PEERS` allow several copies to run side by side.
The `--dev-*` commands work only when `TLS_DATA_DIR` is set.

### Not done yet

The "client hears" mode needs a signed virtual microphone driver. Its button in the interface is disabled for now.

---

## Українська

Програма для локальної мережі: керівник бачить і чує працівника, малює підказки на його екрані,
говорить йому в гарнітуру, надсилає текст і за потреби керує мишею та клавіатурою.

Вимоги: Windows 10 версії 2004 або новіша, Windows 11. Один файл `TLSupporter.exe`.

Стан: перша версія для тестування. Репозиторій закритий, ліцензії поки немає, усі права застережено.
Програма безкоштовна, вільну ліцензію планується додати пізніше.

### Можливості

- Список працівників онлайн, вигляд списком або мініатюрами, групи для кількох команд.
- Перемикання на льоту: лише звук, економ-режим або повний екран.
- Керівник чує звук комп'ютера працівника і його мікрофон.
- Мікрофон керівника працює постійно або як рація, завжди стартує вимкненим, працівник чує його в обох вухах.
- Кілька керівників одночасно, біля кожного малюнка видно ім'я автора.
- Малюнки поверх екрана: два маркери, рамка, стрілка, лазер. Малюнки поступово розчиняються.
- Текстові підказки, набори готових фраз, кнопка «Потрібна допомога» з необов'язковою причиною.
- Керування мишею і клавіатурою, яке керівник вмикає і вимикає сам. Буфер обміну спільний лише при повному підключенні.
- Повний доступ на екрані блокування, екрані входу і у вікнах адміністратора, окреме право для кожного керівника.
- Права керівників, журнал дій, блокування консолі й автоблокування, адмінський пароль на ПК працівника.
- Додавання ПК за одноразовим кодом, пресети для швидкого встановлення, резервна копія налаштувань.
- Автозапуск як служба Windows, у працівника лише значок у треї.
- Хрестик вікна ховає програму в трей. Повністю вона закривається лише через меню значка в треї або налаштування, з адмінським паролем команди.
- ПК керівника можна перевести в режим працівника і повернути назад з адмінським паролем команди.
- Оновлення: ПК керівника завантажує підписану версію з GitHub і роздає її в локальній мережі.

### Як це влаштовано

Окремого сервера немає, дані про команду зберігаються на самих ПК і синхронізуються між ними.
Кожен ПК має власний ключ, тому зміна IP-адреси не заважає підключенню. З'єднання шифрується (QUIC, TLS 1.3)
і приймається лише від адрес локальної мережі. Поки керівник не підключився, екран і звук нікуди не передаються.

Стек: Rust, egui, quinn, Opus, DXGI Desktop Duplication, WASAPI.

### Стан перевірки

Перевірено двома копіями на одному ПК: пошук за ключем, код приєднання, екран, мініатюри, малюнки, текстові підказки,
керування, голос, звук комп'ютера, перемикання на захищений канал при імітації блокування.

Ще не перевірено на реальних ПК: встановлення служби, автозапуск, брандмауер, робота між двома комп'ютерами,
справжні екрани блокування, входу і вікна адміністратора, малюнки й керування поверх mstsc на весь екран, випуск оновлення.

### Встановлення

1. Запусти `TLSupporter.exe` на ПК керівника, вибери «Створити нову команду», підтвердь запит адміністратора.
2. У керівника: Налаштування → Додати ПК → Створити код.
3. На ПК працівника запусти той самий файл, вибери «Приєднатись за кодом».
4. У керівника натисни «Підтвердити» біля нового ПК.

Тихе встановлення з пресетом (від імені адміністратора):

    TLSupporter.exe --install --preset agent.tlpreset --preset-password ПАРОЛЬ

Видалення (від імені адміністратора): `TLSupporter.exe --uninstall`

Мережа: UDP 47700–47703, правило брандмауера додається лише для локальної підмережі.

Повний доступ (екран входу, блокування Windows, вікна адміністратора) вмикається окремо для кожного керівника:
Налаштування → Права керівників. Його обслуговує окремий процес із правами SYSTEM, який запускає служба.
Під час встановлення вмикається політика `SoftwareSASGeneration=1`, щоб працювала кнопка Ctrl+Alt+Del.
Дані зберігаються в `C:\ProgramData\TLSupporter`, журнал роботи у файлі `tlsupporter.log`.

### Збірка

Потрібні Rust, Visual Studio Build Tools 2022 і CMake (є у складі Build Tools, треба додати в PATH):

    cargo build --release

Результат: `target\release\TLSupporter.exe`.

### Випуск оновлення

Закритий ключ підпису лежить поза проєктом: `%USERPROFILE%\.tlsupporter\release_private.key`.
Без нього оновлення випустити неможливо, тому зроби його резервну копію.

    TLSupporter.exe --sign-release TLSupporter.exe %USERPROFILE%\.tlsupporter\release_private.key 0.2.0

Завантаж `TLSupporter.exe` і `TLSupporter.exe.sig` у GitHub Release із тегом `v0.2.0`.
Випуски мають бути в публічному репозиторії, у налаштуваннях вказується `власник/назва`.
Нові поля в протоколі додавай лише так, щоб старі версії могли їх ігнорувати (див. `src/proto.rs`).

### Перевірка двох копій на одному ПК

Змінні `TLS_DATA_DIR`, `TLS_QUIC_PORT`, `TLS_DISCOVERY_PORT`, `TLS_PEERS` дають запустити кілька копій поруч.
Команди `--dev-*` працюють лише коли задано `TLS_DATA_DIR`.

### Ще не зроблено

Режим «чує клієнт» потребує підписаного драйвера віртуального мікрофона. Кнопка в інтерфейсі поки неактивна.
