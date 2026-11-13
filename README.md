# TL Supporter

[English](#english) · [Українська](#українська)

---

## English

A local network tool for team leads: see and hear an employee, draw hints on their screen,
talk into their headset, send text and take over the mouse and keyboard when needed.

Requirements: Windows 10 version 2004 or later, Windows 11. A single file, `TLSupporter.exe`.

Status: version 0.3 for testing. There is no license yet, all rights are reserved.
The program is free of charge, a free license is planned for later.

### Features

- List of employees online, as a list or thumbnails, with groups for several teams.
- Switch on the fly: audio only or the screen in one of four quality levels. A session opens at maximum quality.
- The lead hears the employee's computer sound and microphone.
- The lead's microphone runs constantly or as push-to-talk, always starts off, and the employee hears it in both ears.
- Several leads at once, with the author's name next to each drawing.
- On-screen drawing: two markers, rectangle, arrow with adjustable size. Frames and arrows appear when the button is released, a right click cancels. The laser is a live pointer that leaves no drawing, and clicks show as short rings.
- Text hints with Accept, Reject and Ignore, an unread badge and a shared history kept only while the program runs. Sets of ready phrases, a "Need help" button with an optional reason, Do not disturb and muting of a single PC.
- Mouse and keyboard control that the lead turns on and off at will. Text in the clipboard is shared only while the lead controls the PC. The lead has a mixer for the employee's microphone and the computer sound, the employee can set the volume of the lead's voice or mute it.
- Full access on the lock screen, the sign-in screen and administrator windows, as a separate right for each lead.
- Lead rights, an action journal, console lock and auto-lock, an admin password on the employee's PC.
- Every lead sets a console password of their own on the first start, the team admin password works as a reserve. The console can lock when the program starts, and Enter unlocks it.
- A lead without the Administration right sees the journal of their own groups only.
- The main PC always sees the lead PCs that have no group yet, in "Керівники без групи" (leads without a group).
- The help button can be turned off for a whole group (the "?" button in the group header) or by the employee in their settings. When it is off, it appears for about 20 seconds after a lead's hint and stays while something is unread or a request waits.
- A session can open in a separate window. The session toolbar has a separate button for the picture, and the click rings can be turned off.
- On an employee's PC with several monitors the lead picks the monitor, or turns on "follow the cursor" and the session shows the monitor where the employee's cursor is.
- The main PC confirms the role, access, rights and groups of every PC with its signature. A change that gives more, made by another lead, takes effect on the other PCs once the main PC is online and confirms it. Taking rights away needs no confirmation.
- Settings show the connection code of the PC behind the team admin password, on employee and lead PCs. A lead types it into "Додати ПК за кодом" to move a PC that is already in the team into another group, and the PC keeps its role.
- The version is shown in the console, next to every PC, in the employee's tray tooltip and settings.
- Adding a PC with a one-time code. An employee's PC can be made a lead with the team admin password. Only the main PC, the one that created the team, can give a lead the Administration right. When a lead revokes a PC's access, the program on that PC resets itself and shows a join code again. A factory reset releases the PC from its team. A PC that was reset with version 0.1.3 or older needs `TLSupporter.exe --clear-secure-pin`, run as administrator, before it joins another team.
- Autostart as a Windows service, the employee sees only a tray icon.
- The close button hides the program to the tray and ends the open sessions. The minimize button keeps it on the taskbar. It closes completely only from the tray menu or settings, with the team admin password.
- A lead's PC can switch to employee mode and back with the team admin password.
- Dark interface in English, Ukrainian or Russian. A light theme is available in the settings.
- Updates: the main PC checks GitHub by itself several times a day, downloads a signed version and distributes it over the local network. A PC that was off gets the new version from a lead with the Administration right as soon as it comes online. Every PC checks the release signature before installing.

### How it works

There is no dedicated server. Team data is stored on the PCs themselves and synchronized between them.
Each PC has its own key, so a changing IP address does not break the connection. Connections are encrypted (QUIC, TLS 1.3)
and accepted only from local network addresses. Until a lead connects, screen and audio are not sent anywhere.

Stack: Rust, egui, quinn, Opus, DXGI Desktop Duplication, WASAPI.

### Testing status

Tested with copies of the program on one PC: discovery, join code, screen, thumbnails, drawings, text hints,
control, voice, computer sound, the secure channel on a simulated lock, promotion to a lead, factory reset,
and attacks on the network protocol and on the data files.
Tested on one real PC with an earlier version: service installation, SYSTEM process, firewall rule, restart after the program is killed.

Also tested with copies on one PC: version 0.3.0 passed to a PC that still runs 0.2.0, refused without a valid signature, factory reset through the running program with the leads told about it, moving a PC into another group by its code.

Version 0.3.1, tested with copies on one PC: confirmation of rights by the main PC, version 0.3.1 passed to a PC that runs 0.2.0.

Not yet tested: two real computers, the service of version 0.3, real lock and sign-in screens and administrator windows,
drawing and control over full-screen mstsc, the automatic check against GitHub on a real main PC, the service starting
again by itself after an update, switching monitors on a PC with two of them.

An employee's PC updated over the network by the service of version 0.2.0 or 0.3.0 stays without the program until it is
restarted: those versions do not start themselves again after an update. From 0.3.1 on the service does.

### Installation

You need a Windows account with administrator rights. Run every command below in PowerShell opened as administrator
(Start → type `PowerShell` → Run as administrator).

**Quick way.** Download [install.ps1](install.ps1) and run `powershell -ExecutionPolicy Bypass -File .\install.ps1`. It creates the folder, adds the Defender exclusions, downloads the program and installs it so that it starts with Windows. The steps below do the same by hand.

**1. Add the program to the Windows Defender exclusions.** Do this on every PC before the file gets there.
The program controls a computer remotely and has no publisher signature yet, so Defender may quarantine it as a trojan.

    New-Item -ItemType Directory -Force "C:\TLSupporterSetup" | Out-Null
    Add-MpPreference -ExclusionPath "C:\TLSupporterSetup"
    Add-MpPreference -ExclusionPath "C:\Program Files\TLSupporter"

Check the result. Both folders must be listed:

    Get-MpPreference | Select-Object -ExpandProperty ExclusionPath

If the PC uses another antivirus, add the same two folders in its settings.

**2. Put the file into that folder.** Download [TLSupporter.exe](https://github.com/SemkoArtem/TL_Supporter/releases/latest/download/TLSupporter.exe)
from the latest release, or copy it from a USB drive.
Save it as `C:\TLSupporterSetup\TLSupporter.exe`, then allow it to run:

    Unblock-File "C:\TLSupporterSetup\TLSupporter.exe"

**3. Start it.** The program installs itself into `C:\Program Files\TLSupporter`, registers the service and opens the firewall for the local subnet.

    Start-Process "C:\TLSupporterSetup\TLSupporter.exe"

- First lead: choose "Створити нову команду" (create a new team) and set the team admin password. Keep this password, it is needed to close the program.
- Every other PC: the first-run window shows a connection code. On a lead's PC, type it into "Додати ПК за кодом" (add a PC by code) on the main screen and press "Перевірити" (check). When the PC is found, choose its group and press "Додати в групу" (add to group).

**4. Check it.** The status must be `Running`, and three `TLSupporter` processes must be running:

    Get-Service TLSupporter
    Get-Process TLSupporter

#### Installing a new version

Run `install.ps1` again, or let a lead with the Administration right update the whole team: Settings → Updates → Check and distribute.
By hand: replace the file in `C:\TLSupporterSetup`, then run the commands below. The team and settings are kept.

    Unblock-File "C:\TLSupporterSetup\TLSupporter.exe"
    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--uninstall" -Wait
    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--install" -Wait

#### Removing the program

**Quick way.** Download [uninstall.ps1](uninstall.ps1) and run `powershell -ExecutionPolicy Bypass -File .\uninstall.ps1`. It asks for administrator rights and a confirmation, then removes the service, the program, the firewall rule, the Defender exclusions and all data, including the team of this PC. With `-Check` it only lists what is installed. Afterwards a lead should revoke this PC's access in the console.

By hand:

    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--uninstall" -Wait
    Remove-MpPreference -ExclusionPath "C:\TLSupporterSetup"
    Remove-MpPreference -ExclusionPath "C:\Program Files\TLSupporter"

The team and settings stay in `C:\ProgramData\TLSupporter`. To erase them too:

    Remove-Item -Recurse -Force "C:\ProgramData\TLSupporter"

#### Closing and minimizing

- The close button hides the program to the tray near the clock. The icon may be under the arrow there. The minimize button keeps it on the taskbar.
- A left click on the icon expands the program.
- A right click opens the menu: "Розгорнути" (expand), "Налаштування…" (settings), "Перезапустити" (restart), "Закрити програму…" (close the program). Closing asks for the team admin password, and the program then stays closed until the next Windows sign-in.
- Ending it in Task Manager does not help: the service starts it again within a few seconds.

#### Factory reset

Needed when a PC already belongs to a team, for example when it was set up as a lead by mistake. On a lead's PC: Налаштування (Settings) → Блокування і паролі (Lock and passwords) → Скидання до заводських (factory reset). On an employee's PC: tray icon → Налаштування (Settings) → admin password. The running program does the reset itself and tells the leads, who take the PC off their lists. The team, PC list, journal and settings are erased from this PC, its key stays, and the first-run window opens.

#### If something goes wrong

- Defender blocked it: Windows Security → Protection history. Check that step 1 was done and install again.
- The program log is `C:\ProgramData\TLSupporter\tlsupporter.log`. The button "Відкрити журнал програми" (open the program log) in the settings shows it. The service log is `C:\Program Files\TLSupporterData\system.log`.

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

    TLSupporter.exe --sign-release TLSupporter.exe %USERPROFILE%\.tlsupporter\release_private.key 0.3.1

Upload `TLSupporter.exe` and `TLSupporter.exe.sig` to a GitHub Release tagged `v0.3.1`.
The program looks for releases in `SemkoArtem/TL_Supporter` unless another `owner/name` is set in the settings.
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

Стан: версія 0.3 для тестування. Ліцензії поки немає, усі права застережено.
Програма безкоштовна, вільну ліцензію планується додати пізніше.

### Можливості

- Список працівників онлайн, вигляд списком або мініатюрами, групи для кількох команд.
- Перемикання на льоту: лише звук або екран в одній із чотирьох якостей. Сеанс відкривається з максимальною якістю.
- Керівник чує звук комп'ютера працівника і його мікрофон.
- Мікрофон керівника працює постійно або як рація, завжди стартує вимкненим, працівник чує його в обох вухах.
- Кілька керівників одночасно, біля кожного малюнка видно ім'я автора.
- Малюнки поверх екрана: два маркери, рамка, стрілка з налаштуванням розміру. Рамка і стрілка з'являються після відпускання кнопки, правий клік скасовує. Лазер це живий вказівник без малюнка, а кліки видно як короткі кільця.
- Текстові підказки з кнопками «Прийняв», «Відхилив», «Ігнор», значком непрочитаного і спільною історією, що зберігається лише поки працює програма. Набори готових фраз, кнопка «Потрібна допомога» з необов'язковою причиною, режим «Не турбувати» і вимкнення запитів від окремого ПК.
- Керування мишею і клавіатурою, яке керівник вмикає і вимикає сам. Текст у буфері обміну спільний лише під час керування. У керівника є мікшер мікрофона працівника і звуку системи, працівник може змінити гучність голосу керівника або вимкнути його.
- Повний доступ на екрані блокування, екрані входу і у вікнах адміністратора, окреме право для кожного керівника.
- Права керівників, журнал дій, блокування консолі й автоблокування, адмінський пароль на ПК працівника.
- Кожен керівник під час першого запуску задає власний пароль консолі, адмінський пароль команди підходить як запасний. Консоль може блокуватися під час запуску програми, Enter розблоковує.
- Керівник без права «Адміністрування» бачить журнал лише своїх груп.
- Головний ПК завжди бачить ПК керівників, яких ще не додано в групу, у розділі «Керівники без групи».
- Кнопку допомоги можна вимкнути для всієї групи (кнопка «?» у заголовку групи) або працівнику в його налаштуваннях. Вимкнена кнопка з'являється приблизно на 20 секунд після підказки керівника і лишається, поки є непрочитане або запит чекає.
- Сеанс можна відкривати в окремому вікні. На панелі сеансу є окрема кнопка зображення, кільця кліків можна вимкнути.
- На ПК працівника з кількома моніторами керівник вибирає монітор або вмикає «слідувати за курсором», і сеанс показує монітор, де зараз курсор працівника.
- Головний ПК підтверджує своїм підписом роль, доступ, права й групи кожного ПК. Зміна, що дає більше, зроблена іншим керівником, діє на інших ПК, щойно головний ПК буде в мережі й підтвердить її. Забрати права можна без підтвердження.
- У налаштуваннях за адмінським паролем команди видно код підключення ПК, і на ПК працівника, і на ПК керівника. Керівник вводить його в «Додати ПК за кодом», щоб перенести ПК, який уже в команді, в іншу групу. Роль ПК при цьому зберігається.
- Версію програми видно в консолі, біля кожного ПК, у підказці значка в треї і в налаштуваннях працівника.
- Додавання ПК за одноразовим кодом. ПК працівника можна зробити керівником за адмінським паролем команди. Право «Адміністрування» дає лише головний ПК, той, що створив команду. Коли керівник скасовує доступ ПК, програма на ньому сама скидається і знову показує код для приєднання. Скидання до заводських звільняє ПК від команди. Для ПК, скинутого у версії 0.1.3 або старішій, перед приєднанням до іншої команди запусти від адміністратора `TLSupporter.exe --clear-secure-pin`.
- Автозапуск як служба Windows, у працівника лише значок у треї.
- Хрестик ховає програму в трей і завершує відкриті сеанси. Кнопка згортання лишає її на панелі задач. Повністю вона закривається лише через меню значка в треї або налаштування, з адмінським паролем команди.
- ПК керівника можна перевести в режим працівника і повернути назад з адмінським паролем команди.
- Темний інтерфейс англійською, українською або російською. Світла тема вмикається в налаштуваннях.
- Оновлення: головний ПК сам кілька разів на день перевіряє GitHub, завантажує підписану версію і роздає її в локальній мережі. ПК, що був вимкнений, отримує нову версію від керівника з правом «Адміністрування», щойно з'явиться в мережі. Кожен ПК перевіряє підпис випуску перед встановленням.

### Як це влаштовано

Окремого сервера немає, дані про команду зберігаються на самих ПК і синхронізуються між ними.
Кожен ПК має власний ключ, тому зміна IP-адреси не заважає підключенню. З'єднання шифрується (QUIC, TLS 1.3)
і приймається лише від адрес локальної мережі. Поки керівник не підключився, екран і звук нікуди не передаються.

Стек: Rust, egui, quinn, Opus, DXGI Desktop Duplication, WASAPI.

### Стан перевірки

Перевірено копіями програми на одному ПК: пошук, код приєднання, екран, мініатюри, малюнки, текстові підказки,
керування, голос, звук комп'ютера, захищений канал при імітації блокування, підвищення до керівника, скидання до заводських,
а також атаки на мережевий протокол і файли даних.
Перевірено на одному реальному ПК зі старішою версією: встановлення служби, процес SYSTEM, правило брандмауера, перезапуск після примусового закриття.

Також перевірено копіями на одному ПК: передача версії 0.3.0 на ПК, де ще працює 0.2.0, відмова без дійсного підпису, скидання до заводських через працюючу програму з повідомленням керівникам, перенесення ПК в іншу групу за його кодом.

Версія 0.3.1, перевірено копіями на одному ПК: підтвердження прав головним ПК, передача версії 0.3.1 на ПК з 0.2.0.

Ще не перевірено: два реальні комп'ютери, служба версії 0.3, справжні екрани блокування, входу і вікна адміністратора,
малюнки й керування поверх mstsc на весь екран, автоматична перевірка GitHub на реальному головному ПК, самостійний
перезапуск служби після оновлення, перемикання моніторів на ПК із двома моніторами.

ПК працівника, оновлений по мережі службою версії 0.2.0 або 0.3.0, лишається без програми до перезавантаження: ці
версії після оновлення самі не запускаються знову. Починаючи з 0.3.1 служба робить це сама.

### Встановлення

Потрібен обліковий запис Windows із правами адміністратора. Усі команди нижче виконуй у PowerShell, відкритому від імені адміністратора
(Пуск → введи `PowerShell` → Запуск від імені адміністратора).

**Швидкий спосіб.** Завантаж [install.ps1](install.ps1) і виконай `powershell -ExecutionPolicy Bypass -File .\install.ps1`. Скрипт створить папку, додасть виключення Defender, завантажить програму і встановить її в автозавантаження Windows. Кроки нижче роблять те саме вручну.

**1. Додай програму у виключення Windows Defender.** Зроби це на кожному ПК ще до того, як файл туди потрапить.
Програма дистанційно керує комп'ютером і поки не має підпису видавця, тому Defender може покласти її в карантин як троян.

    New-Item -ItemType Directory -Force "C:\TLSupporterSetup" | Out-Null
    Add-MpPreference -ExclusionPath "C:\TLSupporterSetup"
    Add-MpPreference -ExclusionPath "C:\Program Files\TLSupporter"

Перевір результат. У списку мають бути обидві папки:

    Get-MpPreference | Select-Object -ExpandProperty ExclusionPath

Якщо на ПК інший антивірус, додай ті самі дві папки в його налаштуваннях.

**2. Поклади файл у цю папку.** Завантаж [TLSupporter.exe](https://github.com/SemkoArtem/TL_Supporter/releases/latest/download/TLSupporter.exe)
з останнього випуску або скопіюй його з флешки.
Збережи як `C:\TLSupporterSetup\TLSupporter.exe` і дозволь запуск:

    Unblock-File "C:\TLSupporterSetup\TLSupporter.exe"

**3. Запусти.** Програма сама встановиться в `C:\Program Files\TLSupporter`, зареєструє службу і відкриє брандмауер для локальної підмережі.

    Start-Process "C:\TLSupporterSetup\TLSupporter.exe"

- Перший керівник: вибери «Створити нову команду» і задай адмінський пароль команди. Збережи цей пароль, без нього програму не закрити.
- Кожен наступний ПК: початкове вікно показує код для підключення. На ПК керівника введи його в поле «Додати ПК за кодом» на головному екрані і натисни «Перевірити». Коли ПК знайдеться, вибери групу і натисни «Додати в групу».

**4. Перевір.** Стан має бути `Running`, а процесів `TLSupporter` має бути три:

    Get-Service TLSupporter
    Get-Process TLSupporter

#### Встановлення нової версії

Запусти `install.ps1` ще раз або попроси керівника з правом «Адміністрування» оновити всю команду: Налаштування → Оновлення → «Перевірити й розіслати».
Вручну: заміни файл у `C:\TLSupporterSetup` і виконай команди нижче. Команда і налаштування збережуться.

    Unblock-File "C:\TLSupporterSetup\TLSupporter.exe"
    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--uninstall" -Wait
    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--install" -Wait

#### Видалення програми

**Швидкий спосіб.** Завантаж [uninstall.ps1](uninstall.ps1) і виконай `powershell -ExecutionPolicy Bypass -File .\uninstall.ps1`. Скрипт попросить права адміністратора і підтвердження, а тоді видалить службу, програму, правило брандмауера, виключення Defender і всі дані, разом із командою цього ПК. З параметром `-Check` він лише покаже, що встановлено. Після цього керівнику варто скасувати доступ цього ПК у консолі.

Вручну:

    Start-Process "C:\TLSupporterSetup\TLSupporter.exe" -ArgumentList "--uninstall" -Wait
    Remove-MpPreference -ExclusionPath "C:\TLSupporterSetup"
    Remove-MpPreference -ExclusionPath "C:\Program Files\TLSupporter"

Команда і налаштування лишаються в `C:\ProgramData\TLSupporter`. Щоб стерти і їх:

    Remove-Item -Recurse -Force "C:\ProgramData\TLSupporter"

#### Закриття і згортання

- Хрестик ховає програму в трей біля годинника. Значок може бути під стрілкою. Кнопка згортання лишає програму на панелі задач.
- Лівий клік на значку розгортає програму.
- Правий клік відкриває меню: «Розгорнути», «Налаштування…», «Перезапустити», «Закрити програму…». Закриття просить адмінський пароль команди, після цього програма не працює до наступного входу в Windows.
- Диспетчер завдань не допоможе: служба запустить програму знову за кілька секунд.

#### Скидання до заводських

Потрібне, якщо ПК уже належить до команди, наприклад його випадково налаштували як ПК керівника. На ПК керівника: Налаштування → Блокування і паролі → Скидання до заводських. На ПК працівника: значок у треї → Налаштування → адмінський пароль. Скидання виконує сама працююча програма і повідомляє керівникам, які прибирають ПК зі своїх списків. З цього ПК стираються команда, список ПК, журнал і налаштування, ключ ПК лишається, і відкривається початкове вікно.

#### Якщо щось пішло не так

- Defender заблокував: Безпека Windows → Журнал захисту. Перевір, що крок 1 виконано, і встанови знову.
- Журнал програми: `C:\ProgramData\TLSupporter\tlsupporter.log`, його відкриває кнопка «Відкрити журнал програми» в налаштуваннях. Журнал служби: `C:\Program Files\TLSupporterData\system.log`.

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

    TLSupporter.exe --sign-release TLSupporter.exe %USERPROFILE%\.tlsupporter\release_private.key 0.3.1

Завантаж `TLSupporter.exe` і `TLSupporter.exe.sig` у GitHub Release із тегом `v0.3.1`.
Програма шукає випуски в `SemkoArtem/TL_Supporter`, якщо в налаштуваннях не вказано інший `власник/назва`.
Нові поля в протоколі додавай лише так, щоб старі версії могли їх ігнорувати (див. `src/proto.rs`).

### Перевірка двох копій на одному ПК

Змінні `TLS_DATA_DIR`, `TLS_QUIC_PORT`, `TLS_DISCOVERY_PORT`, `TLS_PEERS` дають запустити кілька копій поруч.
Команди `--dev-*` працюють лише коли задано `TLS_DATA_DIR`.

### Ще не зроблено

Режим «чує клієнт» потребує підписаного драйвера віртуального мікрофона. Кнопка в інтерфейсі поки неактивна.
