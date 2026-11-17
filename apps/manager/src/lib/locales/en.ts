// SPDX-License-Identifier: GPL-3.0-only
// English locale of the manager UI copy: the default language and the source of the key
// set. `tr.ts` has the same keys (type `Locale`; `i18n.test.ts` compares the key sets).
// Keyed by the service's snake_case codes where they exist. Every string here is ours
// (CLEANROOM.md); no wording is taken from any other product.

import { FKEY_SEATS } from '../wizard.ts';
import type { CheckOutcome, DeviceKind, StartMode } from '../types.ts';
import type {
  ErrorCode as RemoteErrorCode,
  SeatState as RemoteSeatState,
  ServiceRunState,
  Verbosity
} from '../remote/types.ts';
import type { FieldError as RemoteFieldError } from '../remote/validation.ts';

const t = {
  appTitle: 'Pulsar Seat Manager',
  loading: 'Loading…',
  refresh: 'Refresh',
  product: 'Product status',
  seats: 'Seats',
  seat: 'Seat',
  examplePath: 'C:\\Games\\game.exe',
  seatN: (n: number) => `Seat ${n}`,
  startAll: 'Start seats',
  stop: 'Stop',
  relogon: 'Sign in again',
  serviceDown: 'The Pulsar Seat service cannot be reached. Make sure the service is installed and running; security software may have stopped it.',
  adminNeeded: 'You need to run as administrator for this action.',
  adminToView: 'Restart as administrator to see the seat settings (button above).',
  restartElevated: 'Restart as administrator',
  // Service start / stop / restart (SCM, elevated manager only; restored from Split).
  svc: {
    label: 'Service',
    start: 'Start service',
    stop: 'Stop service',
    restart: 'Restart service',
    starting: 'Starting the service…',
    stopping: 'Stopping the service…',
    restarting: 'Restarting the service…',
    started: 'Service started.',
    stopped: 'Service stopped.',
    restarted: 'Service restarted.',
    needElevated: 'Run the manager as administrator to start or stop the service.',
    pending: 'The service is changing state right now; wait a moment.',
    notInstalled: 'The service is not installed; reinstall Pulsar Seat.',
    confirmStopTitle: 'Stop the service?',
    confirmStop: 'All running seats (local and remote) close. Unsaved work on the seats may be lost.',
    confirmRestartTitle: 'Restart the service?',
    confirmRestart: 'All running seats close. You may need to start the seats again after the service is back.'
  },
  ok: 'OK',
  cancel: 'Cancel',
  close: 'Close',
  save: 'Save',
  discard: 'Continue without saving',
  yes: 'Yes',
  no: 'No',
  apply: 'Apply',
  later: 'Later',
  none: '—',
  linkDown: 'No live link to the service; the status refreshes every few seconds.',
  restricted: 'An administrator set this PC so that only administrators can see and change the settings.',
  errorJson: 'The text is not valid JSON.',
  osBuildChanged: 'Windows was updated. Review the seat settings; if something is wrong, turn Pulsar Seat off and on.',
  changedElsewhere: 'The settings were changed from another window in the meantime. Saving will not overwrite those changes; reload first.',
  reload: 'Reload',
  seatOneAdvice: 'For stable operation, give the first monitor of the first graphics card (1.1) to seat 1.',
  notAdmin: 'Read-only view: to make changes you must be an elevated administrator.',

  tabs: {
    seats: 'Seats',
    basic: 'Basic settings',
    advanced: 'Advanced',
    network: 'Network addresses',
    remote: 'Remote seats',
    status: 'Status'
  },

  /** Seat kinds (config schema 2): shown next to every seat. */
  seatKind: {
    local: 'Local',
    remote: 'Remote',
    localHint: 'A seat used with a monitor, keyboard and mouse plugged into this PC',
    remoteHint: 'A seat used over the network with Pulsar or Moonlight',
    remoteList: 'Remote seats',
    remoteListHint: 'Remote seats get no devices; their settings are on the Remote seats tab.',
    open: 'Edit'
  },

  network: {
    intro: 'Some network programs and LAN games want each seat to use its own IP address. You can give each seat one of this PC’s static IPv4 addresses.',
    hyperv: 'Hyper-V virtual adapters',
    hypervIntro:
      'A seat can get its own virtual network adapter on an internal Hyper-V switch, with the seat’s address on it. The switch and adapters are named “Pulsar Seat…” and are removed when Pulsar Seat is uninstalled.',
    hypervFeature: (s: string) => `Hyper-V: ${({ enabled: 'on', disabled: 'off', enable_pending: 'turning on (restart needed)', unavailable: 'not available on this Windows edition' } as Record<string, string>)[s] ?? s}`,
    hypervEnable: 'Turn Hyper-V on',
    hypervEnableConfirm: 'Turn on the Windows Hyper-V feature? This is a system change and Windows must restart afterwards.',
    hypervRefresh: 'Check',
    hypervCreate: 'Create adapter',
    hypervRemove: 'Remove adapter',
    hypervNoAddress: 'Assign the seat an address first.',
    hypervReboot: 'Restart Windows to finish turning Hyper-V on.',
    bestEffort: 'While the source IP driver is not loaded, Pulsar Seat keeps this setting and applies it only in programs that support it. If the program has its own “bind to this address” setting, set it to the same address too.',
    enforcedByDriver: (seats: string) => `Source IP driver loaded: programs of seat ${seats} use the seat’s address for their connections.`,
    driverReady: 'Source IP driver loaded; a seat’s address is enforced once it signs in.',
    driverMissing: (why: string) => `The source IP driver is not usable (${why}). The address only applies to programs that read the PULSAR_SEAT_SOURCE_IP variable or have their own address setting.`,
    driverReasons: {
      driver_not_loaded: 'driver not loaded',
      driver_busy: 'driver in use by another process',
      driver_version_mismatch: 'driver version does not match',
      driver_pass_through: 'driver in recovery mode',
      map_invalid: 'the seat list could not be sent',
      map_refused: 'the driver did not accept the seat list'
    } as Record<string, string>,
    sharedSession: (seats: string) => `Seat ${seats} shares its session with another seat; its address is not enforced by the driver.`,
    droppedPaths: (n: number) => `${n} path(s) in the program list could not be passed to the driver (unknown drive letter or relative path).`,
    driverScope: 'The driver only touches IPv4 connections; addresses a program picks itself and connections inside this PC (127.x) stay as they are.',
    seat: 'Seat',
    address: 'IP address',
    rule: 'Rule',
    notAssigned: 'Not assigned',
    edit: 'Edit',
    programs: 'Program list (shared by all seats)',
    programsCount: (n: number) => (n === 1 ? '1 program' : `${n} programs`),
    addressHelp: 'To get more addresses: add a second address to the adapter in Network Connections, or add a virtual network adapter in Device Manager.'
  },

  ipDialog: {
    title: (n: number) => `Seat ${n}: IP address`,
    address: 'IP address',
    notAssigned: 'Not assigned',
    noStatic: 'No static IPv4 address was found on this PC. Addresses from DHCP can change, so they are not listed.',
    loadFailed: 'The address list could not be loaded.',
    rule: 'Which programs should use this address?',
    rules: {
      all_programs: 'All programs started on this seat',
      only_listed: 'Only the programs in the list',
      all_except_listed: 'All programs except those in the list'
    } as Record<string, string>,
    list: 'Program list (shared by all seats)',
    title2: 'Title',
    path: 'Program or folder path',
    pathHint: 'A folder means every program inside it.',
    browse: 'Browse',
    browseHint: 'Pick a program or a shortcut (Start menu, desktop or any drive); the program the shortcut points to is added.',
    addFolder: 'Add the programs in a folder',
    folderEmpty: 'No program was found in the chosen folder.',
    pickFailed: 'The program could not be picked.',
    add: 'Add',
    remove: 'Delete',
    enabled: 'Enabled',
    tooLong: (max: number) => `The list is too long; only the first ${max} rows are kept when saving.`,
    invalidPath: 'The path cannot be empty.',
    openNetwork: 'Open Network Connections',
    openDevices: 'Open Device Manager'
  },

  picture: {
    title: (name: string) => `${name}: picture`,
    default: 'Default picture',
    builtin: 'Built-in pictures',
    user: 'Your own pictures',
    userHint: 'As an administrator you can put your own pictures in %ProgramData%\\Pulsar Seat\\pictures, in the sub-folder of the device kind (for example keyboard, mouse). For the picture shown when the device is not in use, add _off to the end of the file name.',
    noUser: 'You have no own pictures for this kind.',
    choose: 'Choose'
  },

  logLevel: {
    title: 'Log detail',
    intro: 'Errors and warnings are always logged. Extra detail helps when hunting a problem; the log file grows faster.',
    info: 'Information',
    debug: 'Debug',
    trace: 'Trace (very detailed)'
  },

  outputs: {
    title: 'Display outputs',
    intro: 'For each monitor, choose the graphics card output that feeds it. Choosing more than one output copies the picture (cloning). Changes apply at the next start or after a restart; Windows may not always honour the request.',
    adapter: 'Graphics card',
    current: 'current',
    automatic: 'Automatic',
    allowClone: 'Allow copying one monitor to several outputs (cloning)',
    forceAtStart: 'Reconnect each monitor to its saved output when seats start',
    softwareCursor: 'Software mouse pointer',
    hdr: 'HDR',
    vrr: 'Variable refresh (VRR)',
    hdrVrrHint:
      'HDR and variable refresh are for the monitors of seat 2 and later (seat 1 uses the Windows settings). HDR needs an HDR-capable monitor and Windows 11; otherwise the picture stays SDR. Copy-protected (HDCP) content is not shown on these seats.',
    followMode: 'Monitor resolution follows the seat’s session',
    followModeChoice: {
      auto: 'Automatic (only monitors outside the desktop)',
      on: 'On (desktop monitors too; undone when the seat stops)',
      off: 'Off (the picture is scaled)'
    } as Record<string, string>,
    cloneNeeded: 'Allow cloning first to use more than one output.',
    forceNote: 'The outputs you choose are already applied every time a seat starts. With this option on, the service re-applies the split of monitors across seats to Windows each time a seat starts (if Windows changed the outputs on its own).',
    none: 'No monitor found.',
    status: {
      assigned: 'Connected to this output when the seat starts',
      forced: 'Will be forced onto the output',
      unassigned: 'Not assigned'
    } as Record<string, string>
  },

  camera: {
    title: (name: string) => `${name}: camera preview`,
    starting: 'Opening the camera…',
    denied: 'Camera access was not granted, or another program is using the camera.',
    notFound: 'This camera was not found among the video devices; showing the first camera.',
    unsupported: 'Camera preview is not available in this window.'
  },

  bar: {
    save: 'Save',
    saveHint: 'Unsaved changes are lost when the manager closes. Some settings need administrator rights.',
    enable: 'Turn Pulsar Seat on',
    disable: 'Turn Pulsar Seat off',
    start: 'Start seats',
    reboot: 'Restart',
    support: 'Diagnostic report',
    discard: 'Undo changes',
    unsaved: 'There are unsaved changes'
  },

  title: {
    onSeat: (n: number) => `Running on seat ${n}`,
    noSeat: 'Seat unknown'
  },

  // Redesign copy (2026-11-17, target/redesign/final): rail, status line, save bar, notices,
  // seats board, settings pages. Buttons and link rows never end in "…"; menu items that open
  // a dialog do (the Ellipsis Rule, DESIGN.md).
  nav: {
    app: 'Application',
    sections: 'Sections',
    seats: 'Seats',
    remote: 'Remote seats',
    settings: 'Settings',
    status: 'Status',
    statusOk: ': service running',
    statusDown: ': service unreachable',
    statusLink: ': reconnecting to the service',
    count: (n: number) => (n === 1 ? '1 seat' : `${n} seats`),
    version: (v: string) => `Version ${v}`,
    location: 'Location',
    onThisPage: 'On this page'
  },

  power: {
    on: 'Pulsar Seat is on',
    off: 'Pulsar Seat is off',
    summary: (running: number, blocked: string) =>
      `${running === 1 ? '1 local seat is' : `${running} local seats are`} running${blocked ? `, ${blocked} cannot start` : ''}`,
    firstOnly: 'Seat 1 is running, no other seat yet',
    offSummary: 'The seats are set up. Once on, they run according to the start mode.',
    openAndStart: 'Turn on and start',
    startAll: 'Start seats',
    stopAll: 'Stop all seats',
    close: 'Turn Pulsar Seat off…'
  },

  saveBar: {
    region: 'Unsaved changes',
    count: (n: number) => (n === 1 ? '1 unsaved change' : `${n} unsaved changes`),
    undo: 'Undo',
    save: 'Save',
    shortcut: 'Ctrl+Shift+S',
    moved: (device: string, seat: string) => `${device} moved to ${seat}.`,
    restarts: (seats: string) => `Saving restarts ${seats}.`,
    nextBoot: 'Takes effect the next time Windows starts.',
    nextStart: 'The seat uses the new settings the next time it starts.',
    changed: (what: string) => `${what} changed.`
  },

  notices: {
    region: 'Notices',
    count: (n: number) => (n === 1 ? '1 notice' : `${n} notices`),
    summary: 'Changes you should review',
    linkLost: 'The link to the service was lost; reconnecting…',
    ok: 'OK',
    compare: 'Compare'
  },

  service: {
    downTitle: 'The service cannot be reached',
    downBody: 'The Pulsar Seat service is not running, or security software may have stopped it.',
    start: 'Start service',
    restart: 'Restart service',
    stop: 'Stop service',
    diagnostics: 'Diagnostic report'
  },

  device: {
    moved: 'Moved, not saved',
    shared: 'Shared: every seat uses it',
    unplugged: 'Not plugged in',
    actions: (name: string) => `${name}: move or details`,
    faceActions: (name: string, id: string) => `${name} (${id}): move or details`,
    identify: 'Show (identify)',
    infoMenu: 'Device details…',
    infoSub: 'The details help when you look for a problem.'
  },

  move: {
    title: 'Move',
    toPool: 'To unassigned devices',
    share: 'Share with all seats',
    seatHint: (n: number) => `Seat ${n}`,
    announce: (name: string, seat: string) => `${name} moved to ${seat}. Not saved yet.`,
    announcePool: (name: string) => `${name} moved to the unassigned devices. Not saved yet.`,
    announceShare: (name: string) => `${name} is now shared by all seats. Not saved yet.`
  },

  slot: {
    keyboardTitle: 'No keyboard.',
    keyboard: 'This seat cannot start without a keyboard. Drag one here from the unassigned devices.',
    displayTitle: 'No screen.',
    display: 'This seat cannot start without a screen. Drag one here from the unassigned devices.',
    mouseTitle: 'No mouse.',
    mouse: 'Without a mouse the seat can only be used with the keyboard.',
    audio: 'No sound device (optional).'
  },

  seatCard: {
    running: 'Running',
    blocked: 'Cannot start',
    off: 'Off',
    restartNeeded: 'Restart required',
    restart: 'Restart',
    actions: (name: string) => `${name}: seat actions`,
    kioskMenu: 'Kiosk seat…',
    ipMenu: 'IP address…',
    removeMenu: 'Delete seat…',
    atLogon: 'at the sign-in screen',
    gpuMenu: 'Graphics card…'
  },

  startup: {
    recommended: 'recommended for the first setup',
    question: 'When should the extra seats start while Pulsar Seat is on?'
  },

  settingsPage: {
    title: 'Settings',
    intro: 'The defaults suit most setups.',
    startup: 'Start',
    seats: 'Seats',
    seatsHelp:
      'Seats that start automatically follow the start mode above. Startup programs, kiosk, redirection and the IP address are in each seat’s menu.',
    autoStart: 'Starts automatically',
    autoStartFor: (seat: string) => `Start automatically: ${seat}`,
    alwaysOn: 'Always starts',
    account: 'Account',
    accountFor: (seat: string) => `Sign-in account: ${seat}`,
    seatMore: (seat: string) => `${seat}: more seat settings`,
    startupAppsMenu: 'Startup programs…',
    kioskMenu: 'Kiosk…',
    redirectMenu: 'Redirection…',
    ipMenu: 'IP address…',
    cpuMenu: 'Processor cores…',
    padsOn: 'Turn the virtual gamepad on',
    padsOff: 'Turn the virtual gamepad off',
    behaviour: 'Behaviour',
    appearance: 'Appearance',
    theme: 'Theme',
    language: 'Language',
    sameAccountHelp: 'Signing in with the same account on two seats can damage files and settings.',
    maintenance: 'Maintenance',
    diagnostics: 'Diagnostic report',
    diagnosticsHelp: 'Add it when you report a problem. It is saved on this PC and sent nowhere.',
    updates: 'Updates',
    updatesHelp: (version: string) => `Version ${version}`,
    import: 'Import another configuration',
    importHelp: 'The device assignment file of another multiseat program',
    advanced: 'Advanced settings',
    advancedHelp: 'Methods, timings, network addresses, processor cores. For experienced users.',
    reset: 'Reset',
    clearPictures: 'Clear all custom pictures',
    clearPicturesHelp: 'The pictures you gave devices are deleted; the default pictures stay.',
    clear: 'Clear',
    resetAll: 'Reset all settings',
    resetAllHelp: 'Seats, assignments, saved passwords and system settings return to their defaults. This cannot be undone.',
    resetButton: 'Reset'
  },

  advancedPage: {
    title: 'Advanced',
    intro: 'These settings are for experienced users. Do not change a setting you are not sure about; nothing applies until you press Save.',
    toc: 'On this page',
    methods: 'Methods',
    sessionMethod: 'Session method',
    otherMethods: 'Other methods',
    experimental: 'experimental',
    unavailable: (why: string) => `Not available: ${why}`,
    timings: 'Timings',
    safety: 'Safety',
    devices: 'Devices',
    network: 'Network addresses',
    networkHelp:
      'Some network programs and LAN games want each seat to use its own IP address. You can give each seat one of this PC’s static IPv4 addresses.',
    programList: 'Program list',
    programListHelp: 'Shared by all seats. A folder means every program in it.',
    loopback: 'Separate port for seats without a password',
    tweaks: 'Optional system settings',
    unitMs: 'ms',
    unitS: 's',
    unitMin: 'min',
    unitTimes: 'times',
    notYet: 'Not available yet',
    hotkeyChange: 'Change',
    hotkeyChangeLabel: 'Change the switch shortcut',
    ipEdit: 'Edit',
    ipEditFor: (seat: string) => `IP address: ${seat}`,
    notAssigned: 'Not assigned',
    allPrograms: 'All programs',
    listedOnly: 'Only the listed programs',
    allButListed: 'All programs except the listed ones',
    outputsHelp: 'The graphics card output that feeds each monitor, cloning, software cursor',
    cpuHelp: 'Core limit per seat',
    cpuAll: 'Every seat uses all cores',
    logLevelHelp: 'Errors and warnings are always written. More detail helps when looking for a problem.'
  },

  statusPage: {
    intro: 'The current state of the service, the seats and the drivers.',
    restrictions: 'Limitations',
    restrictionsHelp: 'Limits of the chosen methods on this PC.',
    fallbackTo: (method: string) => `Fallback: ${method}`,
    ok: 'OK',
    remoteSeat: (name: string) => `${name} (remote)`,
    serviceRow: 'Pulsar Seat service',
    versions: (service: string, manager: string, arch: string) => `Version ${service} · manager ${manager} · ${arch}`,
    productRow: 'Pulsar Seat',
    noStatus: 'The service has not reported its state.',
    windowsSession: (id: number) => `Windows session ${id}`,
    categories: {
      session: 'Session',
      display: 'Display',
      input: 'Input',
      audio: 'Sound',
      gates: 'Device access'
    } as Record<string, string>,
    notUsed: (wanted: string, why: string) => `${wanted} cannot be used: ${why}`,
    noRestrictions: 'The chosen methods have no known limit on this PC.',
    allMethods: 'Every method on this PC',
    allMethodsHelp: 'What this PC can use, and why the other methods are not available.',
    inUse: 'In use',
    selectedNotInUse: 'Selected, not in use',
    driversHelp: 'Pulsar Seat drivers are not signed yet; user-mode methods are used.'
  },

  /** `DisplayInfo.output_technology` (seat_display::model::output_technology_name). */
  outputTech: {
    vga: 'VGA',
    dvi: 'DVI',
    hdmi: 'HDMI',
    internal: 'Built-in screen',
    displayport: 'DisplayPort',
    miracast: 'Miracast (wireless)',
    indirect: 'Indirect display',
    other: 'Other'
  } as Record<string, string>,

  /** `AppInfo.arch` / update `arch`: Rust target arch names. */
  archNames: { aarch64: 'ARM64', x86_64: 'x64', x86: 'x86' } as Record<string, string>,

  canvas: {
    system: 'Unassigned devices',
    systemHint: 'Devices that no seat has, or that all seats share.',
    moveHint: 'To move a device without dragging, focus it, press Enter and choose the seat in the “Move” menu.',
    moved: (name: string, seat: string) => `${name} moved to ${seat}. Not saved yet.`,
    remoteHint: 'Remote seats get no devices; their settings are on their own pages.',
    addSecondTitle: 'Add a second seat',
    addSecondBody: 'The wizard turns each screen into a seat and finds each seat’s keyboard and mouse when you press a key. It takes about two minutes.',
    startWizard: 'Start the setup wizard',
    addEmpty: 'Add an empty seat',
    more: 'Seats: more actions',
    windowsSettings: 'Windows settings',
    emptySeat: 'No device has been given to this seat yet.',
    addSeat: 'Add seat',
    seatLimit: 'Seat limit reached (16).',
    pickByTouch: 'Find by touch',
    pickByTouchHint: 'Press a key on the keyboard you want to find, or move the mouse.',
    pickCancel: 'Stop searching',
    pickFound: (name: string) => `Found: ${name}`,
    identifyMonitors: 'Number the monitors',
    identifyMonitorsStop: 'Hide the numbers',
    noDevices: 'The device list could not be loaded.',
    sharedSign: 'Shared by all seats',
    spareSign: 'Spare',
    unplugged: 'Not plugged in',
    justConnected: 'Just plugged in',
    attention: 'Needs attention',
    inherited: (hub: string) => `Inherited from hub ${hub}`,
    inheritedPort: (port: string) => `Inherited from the port it is plugged into (${port})`,
    inheritedContainer: 'Inherited from the assignment of the whole device',
    assignPort: 'This port',
    assignContainer: 'The whole device',
    defaultMask: 'Default assignment',
    hubDepth: (n: number) => `Hub depth ${n}`,
    dropHere: 'Drop here',
    menu: 'Menu',
    deviceMenu: (name: string) => `Menu for ${name}`,
    seatMenu: (n: number) => `Seat ${n} menu`
  },

  health: {
    not_starting: 'Will not start',
    no_display: 'No monitor',
    no_keyboard: 'No keyboard',
    no_mouse: 'No mouse',
    ready: 'All ready'
  } as Record<string, string>,

  deviceMenu: {
    assign: 'Assign to seats…',
    spare: 'Make spare',
    reset: 'Restore default',
    indicate: 'Show (identify)',
    indicateStop: 'Stop showing',
    showIds: 'Show hardware IDs',
    hideIds: 'Hide hardware IDs',
    path: 'Connection path',
    sharing: 'Sharing state',
    softwareCursor: 'Use a software mouse pointer',
    hardwareCursor: 'Use the hardware mouse pointer',
    picture: 'Change its picture…',
    outputs: 'Display outputs…',
    preview: 'Camera preview',
    moveHere: 'Move the assignment to this port'
  },

  seatMenu: {
    rename: 'Rename…',
    account: 'Sign-in account…',
    relogon: 'Sign in again',
    start: 'Start this seat',
    stop: 'Stop this seat',
    remove: 'Delete seat',
    cpu: 'Processor cores…',
    ip: 'Assign IP address…',
    resetGraphics: 'Restart the graphics driver',
    resetGraphicsSent: 'The graphics driver restart was sent to the seat (the screen may flash).',
    removeKioskUser: 'Delete the seat’s kiosk user…',
    removeKioskUserTitle: (n: number) => `Delete the kiosk user of seat ${n}?`,
    removeKioskUserBody:
      'Pulsar Seat made a Windows user for this seat’s kiosk shell. When the kiosk shell is turned off the user is only disabled and its files (saves, browser profiles) are kept. Deleting it removes the user and all its files for good.',
    removeKioskUserConfirm: 'Delete the user',
    removeKioskUserDone: 'The seat’s kiosk user was deleted.',
    compat: 'Environment and sign-in…'
  },

  systemMenu: {
    compare: 'Compare with the saved configuration…',
    view: 'View settings…',
    cpu: 'Processor cores per seat…',
    hotkey: 'Keyboard/mouse switch shortcut…',
    wizard: 'Setup wizard…',
    reset: 'Reset all settings…',
    import: 'Import another configuration…',
    deviceManager: 'Device Manager',
    displaySettings: 'Display settings',
    networkConnections: 'Network Connections',
    userAccounts: 'User accounts',
    soundSettings: 'Sound settings',
    panels: 'Windows panels',
    clearPictures: 'Clear all custom pictures',
    logLevel: 'Log detail…',
    outputs: 'Display outputs…',
    usbDock: 'USB dock wizard…'
  },

  kinds: {
    monitor: 'Monitor',
    keyboard: 'Keyboard',
    mouse: 'Mouse',
    hid: 'Other input device',
    audio_render: 'Speaker',
    audio_capture: 'Microphone',
    webcam: 'Camera',
    smart_card_reader: 'Smart card reader',
    usb_hub: 'USB hub',
    storage: 'Storage',
    printer: 'Printer',
    serial_port: 'Serial port',
    network: 'Network adapter',
    other: 'Other device'
  } as Record<DeviceKind, string>,

  assign: {
    title: (name: string) => `${name}: seat assignment`,
    exclusiveHint: 'This device can be on only one seat at a time.',
    shareableHint: 'This device can be used on several seats.',
    noSeat: 'No seat (spare)',
    allSeats: 'All seats',
    clearAll: 'None',
    seatOption: (n: number, name: string) => (name === String(n) ? `Seat ${n}` : `Seat ${n} — ${name}`),
    networkHint: 'Network adapters are always shared; shown for information only.',
    target: 'What are you assigning?',
    targetDevice: 'Only this device',
    targetHint: 'This assignment passes to every device it covers, including those plugged in later. Single-seat devices such as keyboards and mice follow it only when exactly one seat is chosen.'
  },

  seatName: {
    title: (n: number) => `Name of seat ${n}`,
    label: 'Name',
    errors: {
      empty: 'The name cannot be empty.',
      too_long: 'The name can be at most 64 characters.',
      control: 'The name cannot contain invisible control characters.'
    } as Record<string, string>
  },

  account: {
    title: (n: number) => `Sign-in account of seat ${n}`,
    logonScreen: 'Show the normal sign-in screen (recommended)',
    autoLogon: 'Sign in automatically with this account',
    local: 'Local account',
    domain: 'Domain account',
    user: 'User',
    domainName: 'Domain',
    userName: 'Account name',
    pickUser: 'Pick a user',
    openAccounts: 'Open user accounts',
    password: 'Password',
    confirm: 'Password (again)',
    mismatch: 'The passwords do not match.',
    emptyOk: 'The password can be left empty.',
    keepPassword: 'Keep the saved password',
    passwordSet: 'Password saved',
    passwordFailing: 'The saved password was rejected at the last sign-in',
    seatOneWarning: 'Automatic sign-in is not recommended on seat 1; use the normal sign-in screen here.',
    disabledUser: 'disabled',
    adminUser: 'administrator',
    invalidUser: 'The account name cannot be empty and cannot contain the characters \\ / " @.',
    invalidDomain: 'The domain name is not valid.',
    usersFailed: 'The local users could not be loaded; you can type the name by hand.',
    savedHint: 'The password goes only to the service, right away; it is never written to files and cannot be shown later.'
  },

  basic: {
    seatTable: 'Seats',
    colSeat: 'Seat',
    colAccount: 'Sign-in account',
    colStart: 'Start automatically',
    colWallpaper: 'Seat wallpaper',
    colVirtualPads: 'Virtual gamepad',
    colGpu: 'Graphics card preference',
    virtualPadsHint:
      'Virtual gamepad: for a gamepad given to this seat (PlayStation, Switch Pro, Xbox or another HID pad), a virtual pad is created that only this seat’s user can open. ViGEmBus or usbip-win2 must be installed (Pulsar Seat does not install them). With “Lock physical pads to their seat” on, the real pad opens only on this seat.',
    colPadIgnore: 'Pads SDL ignores',
    padIgnoreHint:
      'Pads that SDL games on this seat ignore, in 0xVVVV/0xPPPP form, separated by commas. Pads given to other seats are added automatically.',
    padIgnorePlaceholder: '0x045E/0x0B09',
    colState: 'Current state',
    logonScreen: 'Sign-in screen',
    seatOneAlways: 'Seat 1 always runs.',
    startMode: 'Start mode',
    behaviour: 'Behaviour',
    adminOnly: 'Only administrators can see and change the settings',
    ownSeat: 'Users can restart their own seat',
    hideTray: 'Hide the notification area icon',
    askOnStart: 'Ask before starting seats by hand',
    noShowGoodApply: 'Do not show the success message after saving',
    lockScreen: 'Show the seat picture on the lock screen while Pulsar Seat is on',
    sameAccount: 'Same account on several seats',
    sameAllow: 'Allow',
    sameWarn: 'Allow but warn',
    sameDeny: 'Do not allow',
    screensaver: 'Screen saver settings',
    lockScreenSettings: 'Lock screen settings',
    theme: 'Appearance',
    themeSystem: 'System',
    themeLight: 'Light',
    themeDark: 'Dark',
    language: 'Language',
    languageSystem: 'Windows display language',
    languageNames: { en: 'English', tr: 'Türkçe' } as Record<string, string>,
    languageRestart: 'The manager reloads in the chosen language.',
    colApps: 'Startup programs',
    appsNone: 'None',
    appsCount: (n: number) => (n === 1 ? '1 program' : `${n} programs`)
  },

  startupApps: {
    title: (seat: number) => `Startup programs of seat ${seat}`,
    hint: 'When someone signs in on the seat, these programs start inside the seat’s processor limits. Type a full path (for example C:\\Games\\game.exe).',
    program: 'Program path',
    args: 'Arguments',
    add: 'Add program',
    remove: 'Remove',
    notAbsolute: 'A full path is needed (it must start with a drive letter or \\\\server\\share).',
    empty: 'No startup program yet.'
  },

  gpuPref: {
    title: (seat: number) => `Seat ${seat}: graphics card preference`,
    hint: 'The chosen programs open on the preferred graphics card for the seat user (the same entry as the Windows graphics settings). It is a preference; programs still see the other graphics cards. The change applies the next time the program opens.',
    mode: 'Preference',
    modes: {
      off: 'Off (Windows decides)',
      seat_gpu: 'The graphics card that drives the seat’s monitors',
      high_performance: 'High-performance graphics card',
      power_saving: 'Power-saving graphics card'
    } as Record<string, string>,
    program: 'Program (full path, .exe)',
    add: 'Add program',
    remove: 'Remove',
    empty: 'No program.',
    invalid: 'Enter a full .exe path (for example C:\\Games\\game.exe).',
    off: 'Off',
    count: (n: number) => (n === 1 ? '1 program' : `${n} programs`)
  },

  kiosk: {
    column: 'Kiosk',
    off: 'Off',
    count: (n: number) => (n === 1 ? '1 app' : `${n} apps`),
    title: (seat: number) => `Seat ${seat}: kiosk seat`,
    enable: 'Make this seat a kiosk seat',
    intro:
      'A kiosk seat runs in the same Windows session as seat 1; with “each kiosk seat runs as its own Windows user” on (the default) it gets its own user profile, settings and app data. Only the apps below open on the seat’s monitors. Its keyboard and mouse go only to these apps; this fully holds only when seat 1 is a kiosk seat too or the input filter is active.',
    limits:
      'Limits: all seats share the same Windows session and clipboard. Games and apps that read input with raw input, DirectInput or XInput only get the seat’s own devices when per-app isolation (Advanced tab) is on. For full-screen games choose “Full screen” on the app: the game comes to the front, but only one game on the PC can be in true full screen at a time. Store (UWP / MSIX) apps are added by their app ID (AUMID); they may not open on every Windows version. Programs that hand over to an already open copy stay on the seat when they run as the seat’s own user. Sound: if you give the kiosk seat a sound device assigned only to it, the seat’s apps use it as their per-app default device on Windows 10 1803 and later (an undocumented Windows interface; Windows updates can break it, then sound comes from seat 1’s default device). A kiosk seat is not a full lockdown: Windows handles Ctrl+Alt+Del and Win+L before any user-mode program; whichever seat presses them, all seats switch to the security screen, and signing out there closes all seats. The other system keys (Windows key, Win+R, Win+E, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc) from kiosk keyboards do not reach seat 1’s desktop; without the input filter this is done with a best-effort method that tells keyboards apart. To return to the app list, use the “Apps” button on the bar or Ctrl+Alt+Home.',
    appId: 'Store app ID (AUMID, optional)',
    appIdHint: 'Example: Microsoft.WindowsCalculator_8wekyb3d8bbwe!App. When set, no program path is needed.',
    notAppId: 'Invalid app ID (must be in Package!App form).',
    fullscreen: 'Full screen',
    perSeatAccounts: 'Each kiosk seat runs as its own Windows user',
    perSeatAccountsHint:
      'When on, kiosk seats 2 and later run as their own user: the account saved for the seat if it has an account and password, otherwise a hidden local account Pulsar creates (PulsarKiosk02 …). Each seat gets its own profile, settings and app data, but this is not a security boundary: all kiosk seats share one desktop, so a program on one seat can read the others’ keyboard, screen and clipboard. A new profile is created at first start; app data left in seat 1’s user is not moved. When off, all kiosk seats run as seat 1’s user. When a seat stops being a kiosk seat, its Pulsar account is only disabled and its files stay (seat menu: “Delete the seat’s kiosk user…”).',
    account: {
      console: 'User: seat 1’s user',
      seat: (user: string) => `User: ${user}`,
      auto: (user: string) => `User: ${user} (Pulsar account)`,
      personalWarning:
        'This seat’s saved account (a personal or domain account) signs in on a desktop the other kiosk seats share: their programs can read its keyboard, screen and clipboard. Prefer the Pulsar account (remove the saved password).'
    },
    appTitle: 'Tile name',
    program: 'Program path',
    args: 'Arguments',
    autostart: 'Start when the seat opens',
    monitor: 'Monitor (1 = first)',
    add: 'Add app',
    remove: 'Remove',
    empty: 'No app yet.',
    tooMany: 'At most 16 apps can be added.',
    idle: 'Sign out after no activity (minutes, 0 = off)',
    idleBad: 'Enter a number from 0 to 1440.',
    notAbsolute: 'A full path is needed (it must start with a drive letter or \\\\server\\share).',
    seatOneDesktop: 'Seat 1 stays a normal desktop',
    seatOneDesktopHint:
      'When on, kiosk seats do not take the front. Without the input filter, Pulsar tells devices apart and keeps the Windows key, Alt+Tab, Alt+Esc, Ctrl+Esc and Ctrl+Shift+Esc from kiosk keyboards away from seat 1 (seat 1’s own keyboard keeps using them); if Windows allows it, the other keys of kiosk keyboards and clicks of kiosk mice do not go to seat 1 either. Moving a kiosk mouse can still move seat 1’s pointer. With per-app isolation on, seat 1’s apps do not see kiosk devices in raw input. With the input filter active, kiosk seats’ input goes through the service to their own shell only. When off, seat 1 becomes a kiosk seat too and uses its own app list.',
    systemKeys:
      'Ctrl+Alt+Del (the secure attention sequence) and Win+L cannot be blocked in user mode: they affect all seats, whichever keyboard presses them. The other Windows system keys from kiosk keyboards do not reach seat 1 (without the input filter, telling devices apart is best effort).',
    backendOnly: 'Kiosk seats only work with the “Concurrent kiosk seats” session method.',
    audio: {
      routed: (render: string | null, capture: string | null) =>
        'Sound: ' + [render ? `output ${render}` : null, capture ? `microphone ${capture}` : null].filter(Boolean).join(', '),
      noEndpoint: 'Sound: no separate sound device given to the seat (seat 1’s default device)',
      unsupported: 'Sound: no per-app sound device on this Windows version (seat 1’s default device)',
      sharedApp: (seats: number[]) => `The same program is also on seat ${seats.join(', ')}: they share the sound device setting`
    }
  },

  startModes: {
    manual: 'By hand (only with “Start seats”) — safest, recommended at first',
    at_boot: 'When Windows starts',
    after_seat_one_logon: 'When someone signs in on seat 1'
  } as Record<StartMode, string>,

  startModeHints: {
    manual: 'Seats start only with the button in this window or in the notification area.',
    at_boot: 'All seat monitors must be on while the PC starts. A seat whose monitor is off waits until the monitor appears.',
    after_seat_one_logon: 'For PCs whose monitors are not ready at boot: seats start after the first sign-in on seat 1.'
  } as Record<StartMode, string>,

  advanced: {
    banner: 'This tab is for experienced users. Do not change a setting you are not sure about.',
    backends: 'Methods',
    session: 'Session method',
    topology: 'Where seat hosts run',
    display: 'Display method',
    input: 'Input method',
    audio: 'Sound method',
    gates: 'Device access control',
    active: 'in use now',
    selected: 'selected',
    experimental: 'experimental',
    unavailableHere: 'not available on this PC',
    experimentalAcked: 'The experimental warning was acknowledged.',
    experimentalOpen: 'Read the warning again',
    timings: 'Timings',
    startDelay: 'Wait before the first seat starts (ms)',
    betweenSeats: 'Wait between seats (ms)',
    seatTimeout: 'Seat start timeout (s, 30–120)',
    relogonDelay: 'Delay before signing in again after sign-out (ms)',
    audioWait: 'Time to wait for the audio service (s)',
    monitorIdle: 'Turn off the monitor of an idle extra seat after (s, 0 = off)',
    safety: 'Safety',
    uncleanGuard: 'Do not start extra seats automatically if Windows did not shut down cleanly',
    crashLimit: 'Stay in safe mode if the service crashes this many times in a row',
    hotkey: 'Keyboard/mouse switch shortcut',
    hotkeyOff: 'Off',
    hotkeyChange: 'Change',
    systemButtons: 'Power and sleep buttons on extra seats may affect the whole PC',
    padDeviceScope: 'Lock physical gamepads to their seat',
    padDeviceScopeHint:
      'A pad given to a seat can be opened only by that seat’s user; while remote seats run, the other pads stay with the user at this PC. Off by default (system change): it changes the security of the pads’ device nodes and restarts them, together with the device they hang off (a headset or keyboard with a built-in pad restarts too). Changes are undone when the service stops or this is turned off.',
    devices: 'Devices',
    adminsBypass: 'Administrators can access the devices of all seats',
    classDefaults: 'Class-level default access definitions (system change)',
    classDefaultsSoon: 'Off by default. When on, new devices of the ticked classes start closed to normal users until Pulsar Seat opens them to their seat (no window in between). It changes Windows’ device class default access definitions; everything is undone when turned off. Not tested on real hardware yet.',
    classDefaultClasses: 'Device classes covered when this setting is on',
    classTargets: {
      disk_drive: 'Disks',
      cdrom: 'CD / DVD drives',
      volume: 'Volumes',
      ports: 'Serial and parallel ports',
      modem: 'Modems',
      multiport_serial: 'Multi-port serial adapters',
      floppy: 'Floppy drives',
      tape_drive: 'Tape drives',
      medium_changer: 'Medium changers'
    } as Record<string, string>,
    tweaks: 'Optional system settings',
    tweaksHint: 'Each one is recorded and undone when Pulsar Seat is turned off or uninstalled.',
    hideFus: 'Hide fast user switching',
    hideFusConflict:
      'The selected session method (“Real sessions, one at a time”) relies on fast user switching; this setting would turn that method off, so it is not available now.',
    excludeDrivers: 'Leave driver updates out of quality updates',
    palmCheck: 'Touchpad palm detection threshold',
    palmCheckOff: 'Windows default',
    cpu: 'Processor cores',
    cpuOpen: 'Edit cores per seat',
    inputNotIsolated: 'In this layout, keyboard input of the other seats can also reach seat 1’s foreground window. For full isolation choose the “Dedicated host session” layout or wait for the signed input driver.',
    gpuHint: 'A seat can have several monitors, and seats can share the same graphics card.',
    loopbackTitle: 'Separate port for seats without a password',
    loopbackEnabled: 'Use a separate Remote Desktop port for seats without a saved password',
    loopbackHint:
      'Network level authentication is off on this port; it can be reached only from inside this PC (by a firewall rule). It is not used if the firewall is off or a policy ignores local rules. When the setting changes, Remote Desktop Services restart once no Remote Desktop seat is open.',
    loopbackPort: 'Port (1024–65535, not 3389)',
    loopbackAccounts: 'Extra accounts that may sign in through this port (one per line: user or DOMAIN\\user)',
    loopbackAccountsHint: 'Accounts given to seats are always allowed. Administrators cannot sign in through this port.',
    loopbackAllowed: (list: string) => `These accounts can sign in through this port: ${list}.`,
    loopbackNoAccounts: (seats: string) =>
      `No account is allowed for the seats on the sign-in screen (${seats}); these seats cannot start. Enter above the accounts that will sign in on these seats.`,
    loopbackBinding:
      'Windows can bind a Remote Desktop port only to a network adapter, not to the loopback address, so the firewall rule is what keeps this port inside the PC.',
    restartTs: 'Restart Remote Desktop Services',
    restartTsHint: 'Loads a new or changed port at once. Running seats are stopped first.',
    restartTsTitle: 'Restart Remote Desktop Services?',
    restartTsBody:
      'Every running seat (except seat 1) is stopped and signed out first, then Remote Desktop Services restart. Other Remote Desktop connections to this PC are disconnected too. Seats do not start again by themselves.',
    restartTsConfirm: 'Stop seats and restart',
    restartTsDone: (n: number): string => (n ? `Remote Desktop Services restarted (${n} seat(s) stopped)` : 'Remote Desktop Services restarted'),
    restartTsFailed: (reason: string): string =>
      reason === 'restart_needs_reboot'
        ? 'Remote Desktop Services cannot be stopped on this Windows build; restart the PC instead.'
        : 'Remote Desktop Services could not restart (see the service log).'
  },

  backendNames: {
    rds_loopback: 'Concurrent full sessions (Windows Server x64 only, Remote Desktop Session Host)',
    termsrv_wrapper: 'Multiple sessions on client Windows (experimental)',
    fast_user_switch: 'Real sessions, one at a time (fast user switching, one seat at a time)',
    desktops: 'Concurrent kiosk seats (separate desktops in one session)',
    seat_one_console: 'In seat 1’s own session (default)',
    dedicated_host: 'In a dedicated host session (full input isolation)',
    window_presenter: 'Full-screen window (works everywhere)',
    display_core: 'Dedicated display output',
    pulsar_idd: 'Pulsar virtual display driver',
    console_only: 'Console only',
    raw_input_host: 'Raw input, user mode',
    kmdf_filter: 'Pulsar input filter driver',
    none: 'Off',
    rdp_bridge: 'Route to the seat’s own sound device',
    console_shared: 'All sound from seat 1’s devices',
    device_security: 'Device security definitions',
    session_drive_letters: 'Per-session drive letters',
    camera_consent: 'Camera permissions',
    storage_minifilter: 'Storage filter driver',
    printer_acl: 'Printer queue permissions'
  } as Record<string, string>,

  availability: {
    available: 'Available',
    needs_setup: 'Setup needed',
    driver_not_signed: 'The driver is not signed yet and does not load',
    driver_version_mismatch: 'Driver version does not match; update Pulsar Seat',
    unavailable: 'Not available'
  } as Record<string, string>,

  unavailableReasons: {
    edition_unsupported: 'Not supported by this Windows edition',
    arch_unsupported: 'Not available on this processor architecture',
    rds_not_ready: 'The Remote Desktop Session Host role is not ready',
    no_wrapper: 'No installed wrapper was found',
    not_native: 'The wrapper is not built for this processor',
    not_implemented: 'Not in this version yet',
    not_built: 'Not in this build',
    feature_not_built: 'Not in this build',
    os_unsupported: 'Does not work on this Windows version',
    not_client_sku: 'Only on client editions of Windows',
    rdsh_role_missing: 'The Remote Desktop Session Host role is not installed',
    rdp_denied_by_policy: 'Remote Desktop connections are blocked by policy',
    foreign_service_dll: 'The Remote Desktop service does not use the Windows default',
    unknown_termsrv_build: 'This build of the Remote Desktop service is not recognised',
    wrapper_missing: 'No installed wrapper was found',
    reference_product_present: 'Another multiseat product is installed',
    audio_service_not_running: 'The Windows audio service is not running',
    display_core_unavailable: 'No dedicated display output on this PC',
    experimental_not_acknowledged: 'The experimental method warning was not acknowledged',
    fsu_disabled: 'Fast user switching is turned off by a policy',
    console_not_ready: 'The console session is not ready yet'
  } as Record<string, string>,

  /** `Availability::NeedsSetup.what` codes (the service sends language-free ids). */
  needsSetup: {
    specialized_display: 'the monitor must be set up as a dedicated output',
    enable_remote_desktop: 'Remote Desktop must be turned on',
    termservice_start: 'the Remote Desktop service must be started',
    enable_termsrv_wrapper: 'the wrapper you installed must be enabled',
    console_sign_in: 'a user must sign in on seat 1'
  } as Record<string, string>,

  fallback: {
    instead: (wanted: string, used: string) => `${used} is used instead of ${wanted}`,
    notUsable: (wanted: string) => `${wanted} is not usable`,
    forcedByFus: (used: string) => `${used} is used (fast user switching is active)`,
    unknownReason: (code: string) => `unknown reason (${code})`,
    gate: 'Device access control'
  },

  /** Text per pre-flight check id and outcome; the service's `detail` is for logs. */
  checkText: {
    admin: { pass: 'Running as administrator.', fail: 'Administrator rights are needed.' },
    os_build: {
      pass: 'The Windows version is supported.',
      warn: 'The Windows version is newer than the newest tested version.',
      fail: 'The Windows version is too old.'
    },
    arch_native: {
      pass: 'This package is built for the PC’s processor.',
      warn: 'The PC’s processor architecture could not be determined.',
      fail: 'This package is for another processor architecture; install the right package.'
    },
    edition: { info: 'Windows edition reported.' },
    rdsh_role: {
      pass: 'The role is installed.',
      warn: 'The role was not found; seats fall back to fast user switching.',
      info: 'The role state is unknown.'
    },
    secure_boot: { pass: 'On.', info: 'Off or unknown (reported only; Pulsar Seat never changes it).' },
    memory_integrity: { pass: 'On.', info: 'Off or unknown (reported only; Pulsar Seat never changes it).' },
    smart_app_control: { info: 'Smart App Control state reported; when on, unsigned builds may be blocked.' },
    windows_safe_mode: { pass: 'Normal boot.', warn: 'Windows is in Safe Mode: extra seats do not start.' },
    hyperv_vm: { info: 'Needed only for VM seats; Pulsar Seat never turns it on by itself (Network tab; a restart follows).' },
    driver_signing: { info: 'The Pulsar Seat drivers are not signed yet; user-mode methods are used.' },
    reference_product: { pass: 'Not installed.', fail: 'Another multiseat product is installed.' },
    termservice_stock: { pass: 'The Remote Desktop service uses Windows’ own file.', warn: 'The Remote Desktop service does not use the Windows default.' },
    foreign_input_filters: { pass: 'None.', warn: 'Other input filters are present (left untouched).' },
    steam_proxy_dll: { warn: 'An old Steam add-on DLL (version.dll) is next to steam.exe. Pulsar Seat does not remove it; see Advanced > Compatibility.' }
  } as Record<string, Partial<Record<CheckOutcome, string>>>,
  technicalDetail: 'technical detail',

  notes: {
    input_not_isolated: 'Input is not fully isolated',
    seat_one_sees_other_endpoints: 'Seat 1 also sees the other seats’ sound devices',
    display_fallback: 'The picture is shown in a full-screen window (not hidden from seat 1)',
    audio_console_shared: 'Sound comes from seat 1’s devices',
    audio_no_endpoint: 'No sound device is given to this seat; the seat is silent',
    audio_not_bound: 'The seat’s sound could not be set up (see the service log)',
    audio_service_not_running: 'The Windows audio service is not running; the seat is silent',
    render_absent: 'The seat’s speaker or headphones are missing; the seat is silent',
    render_open_failed: 'The seat’s speaker or headphones could not be opened',
    render_lost: 'The seat’s speaker or headphones were unplugged',
    capture_absent: 'The seat has no microphone',
    capture_open_failed: 'The seat’s microphone could not be opened',
    capture_lost: 'The seat’s microphone was unplugged',
    drive_served_as_seat_user:
      'NTFS / ReFS drives are served as the seat’s user (by the seat’s own helper), so the drive’s permissions apply',
    same_account_warning: 'This account is also signed in on another seat',
    one_seat_at_a_time: 'Only one seat is active at a time',
    fus_inactive_frozen:
      'Seats whose turn it is not are frozen: they have no picture, input or sound, and their programs run in the background. Programs that use the graphics card (games, 3D) may lose their device and close when the seat comes back',
    fus_audio_per_user: 'Each seat’s user picks the default sound device once in their own session (Windows stores it per user)',
    kiosk_audio_shared: 'Sound of apps on kiosk seats comes from seat 1’s default sound device',
    kiosk_audio_per_app: 'Apps on kiosk seats use the sound device given to the seat as their per-app default device (undocumented Windows interface)',
    no_user_isolation: 'No user isolation between seats',
    activity_needs_console_listener: 'The console listener is not running: activity is not detected',
    hotkey_needs_filter: 'The shortcut only works with the console listener or the input filter',
    gamepads_shared: 'Gamepads are shared by all seats',
    xinput_pads_shared: 'XInput gamepads are shared by all seats',
    touch_emulated_as_mouse: 'Touch is passed as mouse input',
    driver_not_signed: 'The driver is not signed',
    input_not_routed: 'Input devices are not routed to seats',
    rdp_listener_on_all_interfaces: 'Remote Desktop listens on all network adapters (network level authentication on)',
    // Redirection notes (seat_rdp::policy, HostRedirection.notes).
    clipboard_shared_with_host_session:
      'The clipboard goes through the host session’s clipboard: seat 1’s user and the other seats with the clipboard on use this clipboard too',
    no_seat_drives: 'This seat has no drive that can be redirected',
    drive_without_global_letter: 'The letter of a seat drive is visible only in the seat’s session; this drive is not redirected',
    drive_acl_volume_refused:
      'A drive’s file system changed after it was checked (FAT / NTFS); it is not redirected until the seat connects again',
    drive_letter_moved: 'A drive letter now points to another volume; this drive is not redirected',
    drive_volume_unknown: 'The volume of a drive could not be determined; this drive is not redirected',
    no_seat_readers: 'No smart card reader is given to this seat',
    no_seat_printers: 'No printer with a queue is given to this seat',
    printers_all_queues: 'All printers of the PC are redirected to this seat (those of other seats too)',
    printers_print_as_host: 'Jobs go to redirected printers as the seat’s background account, not as the seat user',
    // Backend probe / apply notes (seat-session, seat-media, seat-service).
    no_per_user_isolation: 'All seats run as the same Windows user (no user isolation)',
    one_kiosk_visible_at_a_time: 'Only one kiosk desktop is visible at a time',
    kiosk_shell_seat_aware_apps_only:
      'On kiosk seats input comes as window messages; games using raw input, DirectInput / XInput only get the seat’s devices with per-app isolation on; full-screen and Store (UWP) apps are supported best effort',
    kiosk_per_seat_accounts: 'Kiosk seats can run as their own Windows users (from seat 2 on): this keeps their files and settings apart, not their security',
    kiosk_users_share_desktop:
      'Kiosk seats share one Windows desktop: a program on one seat can see the screen, read the keyboard and use the clipboard and windows of the others. Do not sign a personal or work account in to a kiosk seat.',
    kiosk_account_fallback: 'A kiosk seat’s own user could not be opened; that seat runs as seat 1’s user (see the service log)',
    no_privacy_from_console: 'The seat’s picture cannot be hidden from seat 1',
    audio_off: 'Sound off',
    shared_fallback_unconfigured: 'No shared sound device chosen; the seat may stay silent',
    using_shared_fallback: 'The seat uses the shared sound device',
    session_reset_on_timeout: 'Disconnected sessions are closed after a while (policy setting)',
    hardware_gpu_not_enabled: 'Remote Desktop sessions do not use the graphics card (policy setting off)',
    rdp_disabled_until_prepare: 'Remote Desktop is off; the preparation step turns it on',
    rds_licensing_not_configured: 'Remote Desktop licensing is not configured; sessions will not open once the grace period ends',
    two_admin_sessions_only: 'This Windows Server allows only two administrator sessions (no Remote Desktop Session Host role)',
    termservice_restarted: 'Remote Desktop Services restarted',
    loopback_listener_created: 'Port for seats without a password created',
    loopback_listener_updated: 'Port for seats without a password updated',
    loopback_listener_firewall_rule: 'Firewall rule added for the port for seats without a password',
    loopback_listener_old_rule_removed: 'Old firewall rule removed',
    termsrv_wrapper_enabled: 'Remote Desktop wrapper enabled',
    wrapper_managed_externally: 'Another program manages the Remote Desktop wrapper; Pulsar Seat does not touch it'
  } as Record<string, string>,

  redirect: {
    column: 'Redirection',
    off: 'Off',
    title: (seat: number) => `Redirection of seat ${seat}`,
    hint: 'These settings fully apply only in the IronRDP seat client. The ActiveX client cannot filter: it passes the clipboard only when everything is on, and never redirects drives or smart cards.',
    seatOne: 'Seat 1 runs in the host session; no redirection needed.',
    clipboard: 'Clipboard',
    directions: {
      off: 'Off',
      to_seat: 'To the seat (paste from the host session into the seat)',
      from_seat: 'From the seat (copy from the seat to the host session)',
      both: 'Both ways'
    } as Record<string, string>,
    short: { off: 'off', to_seat: 'to the seat', from_seat: 'from the seat', both: 'both ways' } as Record<string, string>,
    formats: 'Formats passed',
    text: 'Text',
    html: 'HTML and rich text',
    images: 'Images',
    files: 'Files',
    crossSeat: 'Data copied on other seats may also reach this seat',
    sharedWarning:
      'Warning: the clipboard goes through the host session’s (seat 1’s) clipboard. Seat 1’s user and the other seats with the clipboard on can see this data.',
    drives: 'Drives',
    driveModes: { off: 'Off', read_only: 'Read-only', read_write: 'Read-write' } as Record<string, string>,
    drivesHint:
      'Only drives that belong to this seat alone (for example a USB stick or a second disk) are redirected. FAT32 / exFAT drives are served by the seat’s background account; NTFS / ReFS drives as the seat’s user, so their permissions apply (they appear once the user has signed in). Changes reach a connected seat at once.',
    smartcards: 'Smart card',
    smartcardsLabel: 'Open the smart card readers given to this seat to the seat session',
    smartcardsHint: 'The seat session sees only its own readers. With two readers of the same model, both stay closed if the reader ID cannot be read.',
    printers: 'Printers',
    printerModes: {
      off: 'Off',
      seat: 'Printers given to this seat',
      all: 'All printers of the PC'
    } as Record<string, string>,
    printersHint:
      'The seat session can already see this PC’s printers directly; a redirected printer also shows in the session with a “redirected” tag. Print jobs go as the seat’s background account, not as the seat user. The change applies at the seat’s next connection.',
    printersAllWarning: 'Warning: printers of other seats also show and can be used on this seat.',
    printersShort: { off: '', seat: 'Printer: the seat’s', all: 'Printer: all' } as Record<string, string>,
    summary: (clip: string, drives: string, cards: boolean, printers = '') =>
      [
        clip !== 'off' ? `Clipboard ${clip}` : '',
        drives !== 'Off' ? `Drive: ${drives.toLocaleLowerCase('en')}` : '',
        cards ? 'Smart card' : '',
        printers
      ]
        .filter(Boolean)
        .join(' · ') || 'Off',
    refusals: 'Latest refused redirections',
    channels: { clipboard: 'Clipboard', drives: 'Drive', smartcard: 'Smart card', printers: 'Printer', activex: 'ActiveX client' } as Record<string, string>
  },

  /** Text per refused redirection reason (seat host, `RefusalLog`). */
  refusalReasons: {
    direction_to_seat_off: 'passing to the seat is off',
    direction_from_seat_off: 'passing from the seat is off',
    format_not_allowed_to_seat: 'this format may not be passed to the seat',
    format_not_allowed_from_seat: 'this format may not be passed from the seat',
    format_not_offered: 'the requested format was not offered',
    paste_from_seat_refused: 'paste from the seat refused',
    files_to_seat_off: 'file transfer to the seat is off',
    files_from_seat_off: 'file transfer from the seat is off',
    clipboard_from_other_seat: 'the data was copied on another seat',
    stale_clipboard_response: 'a clipboard reply from an earlier connection was dropped',
    clipboard_backend_missing: 'the clipboard could not start',
    reader_not_assigned: 'the reader does not belong to this seat',
    reader_name_ambiguous: 'the reader name is on several readers; its ID could not be read',
    reader_admin_refused: 'no permission to change reader settings',
    reader_source_missing: 'the smart card service is not available',
    smartcard_off: 'smart card redirection is off',
    list_readers_failed: 'the readers could not be listed',
    drive_letter_moved: 'the drive letter now points to another volume',
    drive_volume_unknown: 'the drive’s volume could not be determined',
    drive_acl_volume_refused: 'the drive’s file system changed after it was checked; not redirected until the next connection',
    drive_unassigned: 'the drive no longer belongs to this seat; removed from the session',
    drive_broker_unavailable: 'the seat’s drive helper is not running yet (the user has not signed in)',
    drive_broker_lost: 'the connection to the seat’s drive helper was lost',
    drive_broker_refused: 'the seat’s drive helper refused the drive',
    drive_add_failed: 'the new drive could not be added to the session',
    printer_unassigned: 'the printer no longer belongs to this seat; removed from the session',
    printer_add_failed: 'the new printer could not be added to the session',
    printer_remove_failed: 'the printer could not be removed from the session',
    drive_remove_failed: 'the drive could not be removed from the session',
    drive_broker_answer_dropped: 'an invalid answer of the seat’s drive helper was dropped',
    list_reader_groups_failed: 'the reader groups could not be listed',
    drive_is_system_drive: 'the Windows drive is not redirected',
    drive_root_invalid: 'invalid drive root',
    drive_root_unavailable: 'the drive could not be opened',
    printer_not_redirected: 'the printer is not redirected to this seat',
    no_printer_queue_found: 'no printer queue found to redirect',
    printer_discovery_failed: 'the printers could not be read',
    port_not_redirected: 'ports are not redirected',
    clipboard_filter_needs_ironrdp: 'the ActiveX client cannot filter the clipboard; clipboard off',
    drives_need_ironrdp: 'the ActiveX client does not redirect drives',
    smartcards_need_ironrdp: 'the ActiveX client does not redirect smart cards',
    printers_need_ironrdp: 'the ActiveX client does not redirect printers'
  } as Record<string, string>,

  drivers: {
    kbd_mou_filter: 'Keyboard/mouse filter',
    hid_filter: 'HID filter',
    idd: 'Virtual display driver',
    storage_minifilter: 'Storage filter',
    source_ip_callout: 'Source IP driver'
  } as Record<string, string>,

  driverStatus: {
    not_installed: 'Not installed',
    installed_not_loaded: 'Installed, not loaded',
    loaded: 'Loaded',
    version_mismatch: 'Version does not match'
  } as Record<string, string>,

  statusTab: {
    service: 'Service',
    version: 'Version',
    managerVersion: 'Manager version',
    arch: 'Architecture',
    revision: 'Configuration revision',
    seats: 'Seat states',
    session: 'Session',
    since: 'State change',
    fallback: 'Fallback notes',
    backends: 'Methods',
    drivers: 'Pulsar drivers',
    checks: 'Pre-flight checks',
    runChecks: 'Run the checks',
    noChecks: 'No check has run yet.',
    caller: 'This window',
    admin: 'Administrator (elevated)',
    notAdmin: 'Standard user',
    degraded: 'Limitations'
  },

  checks: {
    os_build: 'Windows version',
    secure_boot: 'Secure Boot',
    memory_integrity: 'Memory Integrity',
    edition: 'Windows edition',
    rdsh_role: 'Remote Desktop Session Host role',
    termservice_stock: 'Remote Desktop service is stock',
    reference_product: 'Another multiseat product',
    foreign_input_filters: 'Other input filter drivers',
    steam_proxy_dll: 'Old Steam add-on DLL',
    arch_native: 'Native build',
    admin: 'Administrator rights',
    smart_app_control: 'Smart App Control',
    windows_safe_mode: 'Windows Safe Mode',
    driver_signing: 'Driver signature',
    hyperv_vm: 'Hyper-V for VM seats'
  } as Record<string, string>,

  outcomes: {
    pass: 'OK',
    warn: 'Warning',
    fail: 'Problem',
    info: 'Info'
  } as Record<CheckOutcome, string>,

  conflicts: {
    title: 'Assignments that cannot be applied',
    body: 'The assignment of the devices below cannot be applied as it is. You can still save, or cancel and fix them.',
    saveAnyway: 'Save anyway'
  },

  saved: {
    ok: 'Settings saved.',
    restartSeats: (seats: string) => `For the change to take effect these seats must restart: ${seats}.`,
    reboot: 'This change takes effect after the PC restarts.',
    errors: 'Not saved: fix the problems below first.',
    movedBeyond: 'Devices on seats that no longer exist were made spare. Remember to save.'
  },

  unsaved: {
    title: 'Unsaved changes',
    body: 'Your changes are not saved yet.',
    save: 'Save',
    discard: 'Continue without saving',
    cancel: 'Cancel'
  },

  enable: {
    enableTitle: 'Turn Pulsar Seat on?',
    enableBody: 'When Pulsar Seat is on, extra seats run according to the chosen start mode. A diagnostic report is saved before turning it on.',
    disableTitle: 'Turn Pulsar Seat off?',
    disableBody: 'Running extra seats close; unsaved work in open sessions may be lost.',
    rebootTitle: 'Restart needed',
    rebootBody: 'Windows must restart to finish the change. Restart now?',
    rebootNow: 'Restart now',
    rebootFailed: 'The PC could not restart by itself. Please restart it by hand.',
    offerEnable: 'The seats are set up but Pulsar Seat is off. Turn it on and start the seats now?',
    offerEnableButton: 'Turn on and start'
  },

  start: {
    confirmTitle: 'Start the seats?',
    confirmBody: 'All seats marked to start automatically are started one after another.',
    dontAsk: 'Don’t ask again',
    disabled: 'Turn Pulsar Seat on first to start the seats.',
    queued: 'Starting the seats…',
    safeMode: 'Pulsar Seat is in safe mode; seats cannot start.',
    noStatus: 'The service state is not known yet; try again in a moment.'
  },

  inputMoveDialog: {
    title: 'Keyboard and mouse moved',
    body: (seat: number) =>
      `A keyboard (and the mice on the same hub) moved to seat ${seat} with the switch shortcut. The move holds for now; save to keep it. If you cancel, the devices go back to their saved seats.`,
    save: 'Save',
    discard: 'Cancel'
  },

  sameAccountDialog: {
    title: 'The same account is signed in on another seat',
    body: 'This account is also signed in on another seat. Using the same account on several seats is not recommended: files and settings can corrupt each other.',
    dontShow: 'Don’t show again',
    signOut: 'Sign out',
    keep: 'Keep signed in'
  },

  remove: {
    title: (n: number) => `Delete seat ${n}?`,
    titleNamed: (name: string) => `Delete ${name}?`,
    body: 'The devices of this seat become spare. The change takes effect when saved.'
  },

  reset: {
    title: 'Reset all settings?',
    body: 'Seats, device assignments, saved passwords and optional system settings return to their defaults. This cannot be undone.',
    confirm2Title: 'Are you sure?',
    confirm2Body: 'Last confirmation: all Pulsar Seat settings will be deleted.',
    confirm: 'Reset',
    done: 'Settings reset.'
  },

  quit: {
    title: 'Close the manager?',
    body: 'When the window closes, the manager keeps running in the notification area. Do you want to quit the program completely? The seats keep running.',
    confirm: 'Quit'
  },

  tray: {
    hidden: 'The manager keeps running in the notification area. To quit completely, choose “Quit” in the icon’s menu.'
  },

  view: {
    title: 'View of the Seats tab',
    scaleIcons: 'Shrink device pictures to the screen size',
    evenRows: 'Arrange seat boxes in even rows',
    flashSeconds: 'Highlight time of a just plugged-in device (s)',
    showAllHubs: 'Show all USB hubs',
    showAllHubsHint: 'When off, empty hubs and hubs that carry only other hubs are hidden.',
    sharedInSeats: 'Also show shared devices in the seat boxes',
    showIds: 'Show hardware IDs next to device names',
    visibleSeats: 'Seats shown'
  },

  compare: {
    title: 'Comparison with the saved configuration',
    matches: 'The plugged-in devices match the saved configuration.',
    changed: 'The configuration changed since the last save.',
    added: 'Added after the save',
    missing: 'Not plugged in (removed after the save)',
    maskChanged: 'Assignment changed',
    was: (v: string) => `before: ${v}`,
    now: (v: string) => `now: ${v}`,
    restore: 'Return to the saved assignment',
    newMonitorsSpare: 'Make new monitors spare',
    moveAssignment: 'Move the assignment to the new port',
    movedFrom: 'This device seems to be plugged into another port.'
  },

  cpu: {
    title: 'Processor cores per seat',
    hint: 'A seat with nothing ticked uses all cores. Each seat must keep at least one core.',
    hard: 'Hard limit (one processor group)',
    performance: 'performance',
    efficiency: 'efficiency',
    core: (i: number) => `Core ${i}`,
    group: (g: number) => `Group ${g}`,
    all: 'All',
    failed: 'The processor information could not be loaded.',
    oneGroup: 'With a hard limit the chosen cores must be in one processor group.',
    seatWide: 'Whole session',
    seatWideHint:
      'Without “Whole session” the cores apply to the seat’s desktop (Explorer and what starts from it, the Start menu) and the apps Pulsar Seat starts. With it, every other new program of the seat’s session (also programs run as administrator) gets them as its preferred cores within a second; protected system processes are left alone.'
  },

  hotkey: {
    title: 'Keyboard/mouse switch shortcut',
    body: 'The keyboard that presses this shortcut and the mice on its seat move to the next seat. The modifier is always Ctrl.',
    warning: 'Choose a system-wide shortcut with care: do not pick a key that other programs use.',
    press: 'Click the field and press a key',
    invalid: 'This key cannot be used.',
    off: 'Turn the shortcut off',
    current: 'Chosen'
  },

  diagnostics: {
    title: 'Diagnostic report',
    body: 'The report is saved on this PC; it is sent nowhere. Passwords never go into the report.',
    maskUsers: 'Hide user names (recommended)',
    create: 'Create the report',
    done: (path: string) => `Report saved: ${path}`
  },

  import: {
    title: 'Import another configuration',
    body: 'Paste the content of a device assignment file (JSON) exported by another multiseat product. Nothing is saved by itself; review the result and save.',
    file: 'Choose file',
    run: 'Import',
    done: 'The imported configuration is loaded. Review it and save.'
  },

  experimental: {
    title: 'Experimental session method',
    ack: 'I have read and understood the risks',
    enable: 'Choose the experimental method',
    cancel: 'Cancel'
  },

  wizard: {
    title: 'Setup wizard',
    intro: 'The wizard turns each monitor into a seat and finds each seat’s keyboard and mouse when you press a key. Plug in every device before you start, and do not plug in or remove devices while the wizard runs.',
    firstRun: 'No seat is set up yet. Do you want to run the setup wizard?',
    variants: 'How do you want to go on?',
    all_seats: 'Each seat has its own keyboard and mouse: identify all seats (seat 1 too) in turn',
    keep_seat_one: 'Keep seat 1’s current devices: identify only seats 2 and later',
    quick_two_seats: 'Quick two seats: seat 1’s devices on seat 1, everything else on seat 2',
    seatOneDevices: 'Devices to use for seat 1',
    extend: 'Extend the desktop to all monitors (recommended)',
    start: 'Start the wizard',
    pickMonitor: 'Each monitor shows a large number. Press the number shown on seat 1’s monitor (0 for 10) or pick it below.',
    pickMonitorMany: 'For monitors numbered above 10, click the button on that monitor or in the list below.',
    /** F1..F12 only exist up to seat 12; seats 13+ use the any-key way alone. */
    keyboard: (n: number) =>
      n <= FKEY_SEATS
        ? `Press F${n} twice on the keyboard of seat ${n}.`
        : `When seat ${n}’s turn comes, press any key twice on that seat’s keyboard.`,
    anyKey: 'When it is this seat’s turn, pressing any key twice on its keyboard is enough too.',
    invalid: 'This assignment could not be saved. The wizard goes on: fix the problem and save again, or quit without saving.',
    gather: 'Find the main keyboard and mouse by touch',
    gatherStop: 'Stop searching',
    gatherHint: 'Press a key on the keyboard you will use on seat 1 and move the mouse.',
    gathered: 'Main devices found by touch',
    minResolution: (w: number, h: number) => `The main monitor’s resolution must be at least ${w}×${h} for the wizard. Raise the resolution in the display settings.`,
    noMonitor: 'The wizard needs at least one monitor.',
    pagesFailed: 'The monitor pages could not open; the wizard runs only in this window.',
    pageNumber: 'This monitor’s number',
    pagePick: 'If this is seat 1’s monitor, press this number or click the button below.',
    pagePickButton: 'This monitor is seat 1',
    pagePickButtonOnly: 'If this is seat 1’s monitor, click the button below (there is no key for this number).',
    pageSeat: (n: number) => `Seat ${n}`,
    pageNoSeat: 'No seat was given to this monitor (seat limit).',
    pageOtherSeat: (n: number) => `Seat ${n} is being set up now. Instructions show here when this seat’s turn comes.`,
    pageTarget: 'Click this target',
    pageKept: 'This seat’s devices stay as they are.',
    pageDone: 'This seat is done. Waiting for the other seats.',
    lastUnit: (name: string) => `Last detected: ${name}`,
    keyboardCount: (k: number) => `Pressed: ${k} / 2`,
    mouse: (n: number) => `With seat ${n}’s mouse, click the two targets on that seat’s monitor.`,
    mouseCount: (k: number) => `Clicked: ${k} / 2`,
    waiting: 'This seat is done; waiting for the other seats.',
    done: 'All seats are identified.',
    cancelled: 'The wizard closed without saving.',
    keys: 'On any keyboard: Enter confirms this seat · Backspace resets this seat · Shift+Enter saves all · Shift+Backspace resets all · Shift+Esc or Alt+F4 quits without saving',
    keysIntro: 'On any keyboard:',
    moreKeys: 'Other shortcuts',
    /** The legend: `enter` and `shiftEsc` stay visible, the rest sit under `moreKeys`. */
    keysList: {
      enter: { keys: 'Enter', does: 'confirms this seat' },
      shiftEsc: { keys: 'Shift+Esc', does: 'quits without saving' },
      back: { keys: 'Backspace', does: 'resets this seat' },
      shiftEnter: { keys: 'Shift+Enter', does: 'saves all' },
      shiftBack: { keys: 'Shift+Backspace', does: 'resets all' },
      altF4: { keys: 'Alt+F4', does: 'quits without saving' }
    },
    /** Start screen (prototype 40): the variants as label + help. */
    variantsLabel: 'Wizard mode',
    variantOptions: {
      all_seats: { label: 'Each seat has its own keyboard and mouse', help: 'Identify all seats (seat 1 too) in turn.' },
      keep_seat_one: { label: 'Keep seat 1’s current devices', help: 'Identify only seats 2 and later.' },
      quick_two_seats: { label: 'Quick two seats', help: 'Seat 1’s devices stay on seat 1, everything else goes to seat 2.' }
    },
    mainDevices: 'Main keyboard and mouse',
    findByTouch: 'Find by touch',
    currentDevices: 'Seat 1 uses now',
    extendLabel: 'Extend the desktop to all monitors',
    extendHelp: 'Recommended. The wizard writes a number on each monitor.',
    /** Step screen (prototype 41). */
    progress: 'Progress',
    finish: 'Finish',
    /** Heading with the function key drawn inline: [text before the key, text after it]. */
    keyHead: (n: number): [string, string] => ['Press ', ` twice on seat ${n}’s keyboard`],
    seatStep: (n: number, total: number, kind: 'keyboard' | 'mouse') => `Seat ${n} of ${total}, ${kind === 'keyboard' ? 'keyboard' : 'mouse'} step.`,
    pickHead: 'Press the number shown on seat 1’s monitor',
    pickSub: 'Each monitor shows a large number. Press it on any keyboard (0 for 10), or choose the monitor here.',
    pickThis: 'Choose',
    pickThisAria: (name: string) => `Choose ${name} as seat 1’s monitor`,
    lastDetected: 'last detected',
    noneFound: 'No device found yet.',
    /** Per-monitor page (prototype 42). */
    pageKeyHead: (): [string, string] => ['Press ', ' twice on this seat’s keyboard'],
    pagePickHead: 'Is this seat 1’s monitor?',
    pageOwner: (n: number) => `This monitor is seat ${n}`,
    pageKeysList: { enter: 'confirm', shiftEsc: 'quit without saving' },
    confirmSeat: 'Confirm this seat',
    resetSeat: 'Reset this seat',
    resetAll: 'Reset all',
    saveAll: 'Save and finish',
    quit: 'Quit without saving',
    assigned: 'Devices found',
    notAllConnected: 'The wizard needs all devices plugged in. Some saved devices are not plugged in.',
    finishHint: 'When done, remember to choose a start mode and turn Pulsar Seat on.',
    stepNames: {
      pick_console_monitor: 'Seat 1’s monitor',
      keyboard: 'Keyboard',
      mouse: 'Mouse',
      waiting: 'Waiting',
      done: 'Done',
      cancelled: 'Cancelled',
      invalid: 'Not saved'
    } as Record<string, string>
  },

  seatState: {
    off: 'Off',
    waiting_for_display: 'Waiting for a monitor',
    starting: 'Starting',
    at_logon: 'At the sign-in screen',
    signed_in: 'Signed in',
    locked: 'Locked',
    stopping: 'Stopping',
    error: 'Error'
  } as Record<string, string>,

  /** Device descriptions for tooltips and the device info dialog (describe.ts). */
  describe: {
    seatsOf: (seats: number[]) => (seats.length === 1 ? `Seat ${seats[0]}` : `Seats ${seats.join(', ')}`),
    explicit: 'Assigned by hand',
    ids: 'Hardware IDs',
    output: {
      internal: 'built-in',
      hdmi: 'HDMI',
      displayport: 'DisplayPort',
      dvi: 'DVI',
      vga: 'VGA',
      miracast: 'wireless (Miracast)',
      indirect: 'indirect (USB / virtual)',
      other: 'other'
    } as Record<string, string>,
    isolation: {
      descriptor: 'Access is limited by the device security definition',
      session_letter: 'The drive letter is visible only in its owner’s session',
      minifilter: 'Limited by the storage filter',
      camera_consent: 'Limited by the camera permission',
      shared: 'Shared: everyone can use it',
      not_supported: 'This device cannot be closed to a seat on this PC',
      network: 'Network adapter: always shared',
      seat_filter: 'Only its seat’s programs see it (smart card redirect / Pulsar Seat module)'
    } as Record<string, string>,
    seat: (v: string) => `Seat: ${v}`,
    location: (v: string) => `Location: ${v}`,
    adapter: (v: string) => `Graphics card: ${v}`,
    connection: (v: string) => `Connection: ${v}`,
    resolution: (w: number, h: number, hz: string) => `Resolution: ${w}×${h}, ${hz} Hz`,
    ports: (n: number) => `Ports: ${n}`,
    plugged: (v: string) => `Plugged in: ${v}`,
    bus: (v: string, builtIn: boolean) => `Connection type: ${v}${builtIn ? ' (built-in)' : ''}`,
    letters: (v: string) => `Drive letters: ${v}`,
    serial: (v: string) => `Serial number: ${v}`,
    hardwareId: (v: string) => `Hardware ID: ${v}`
  },

  /** Names of special keys in the switch shortcut (hotkey.ts). */
  keyNames: { Backspace: 'Backspace', Tab: 'Tab', Space: 'Space' } as Record<string, string>,

  /** Built-in device picture names (pictures.ts ids). */
  pictureNames: {
    'monitor-speakers': 'Monitor with speakers',
    'monitor-curved': 'Curved monitor',
    tv: 'Television',
    laptop: 'Laptop screen',
    'keyboard-compact': 'Compact keyboard',
    'keyboard-numpad': 'Keyboard with numpad',
    'keyboard-laptop': 'Laptop keyboard',
    'mouse-wireless': 'Wireless mouse',
    'mouse-vertical': 'Vertical mouse',
    trackball: 'Trackball',
    touchpad: 'Touchpad',
    trackpoint: 'Pointing stick',
    pen: 'Pen',
    touchscreen: 'Touch screen',
    gamepad: 'Gamepad',
    headphones: 'Headphones',
    'speaker-single': 'Single speaker',
    headset: 'Headset',
    'webcam-clip': 'Clip-on camera'
  } as Record<string, string>,

  /** Update dialog (update.ts). */
  update: {
    bar: 'Updates',
    title: 'Updates',
    intro:
      'If you want, Pulsar Seat checks for new versions on the release page on GitHub. While the check is off no connection is made; no information about your PC is sent.',
    optIn: 'Check for new versions (GitHub)',
    weekly: 'Check by itself once a week (only notifies, does not download)',
    current: (v: string, arch: string) => `Installed version: ${v}${arch ? ` (${arch.toUpperCase()})` : ''}`,
    lastChecked: (when: string) => `Last check: ${when}`,
    never: 'Not checked yet.',
    check: 'Check now',
    download: 'Download and verify',
    install: 'Install',
    page: 'Release page',
    pageHint: (url: string) => `Release notes: ${url}`,
    state: {
      idle: 'No check done.',
      checking: 'Checking…',
      up_to_date: 'Pulsar Seat is up to date.',
      available: (v: string) => `New version available: ${v}`,
      downloading: (pct: number | null) => (pct === null ? 'Downloading…' : `Downloading… ${pct}%`),
      ready: (v: string, signer: string) => `${v} downloaded and verified (signed by: ${signer}).`,
      installing:
        'The installer package is verified again and installed. The Pulsar Seat service restarts during the installation; the manager connection may drop for a short time.',
      failed: 'The last step failed.'
    },
    confirmTitle: 'Install the update?',
    confirmBody: (v: string) =>
      `Pulsar Seat ${v} will be installed. All running seats will stop and the sessions on the seats will close. Your settings and seat passwords are kept. If needed, you are asked to restart after the installation.`,
    confirmYes: 'Install now',
    confirmNo: 'Cancel',
    needsOptIn: 'Turn on the option above first to check.',
    reasons: {
      disabled: 'The update check is off.',
      network: 'Could not connect to the release page. Check the internet connection.',
      feed: 'The release information could not be read.',
      no_asset: 'The new version has no installer package for this PC’s architecture.',
      no_checksum: 'The new version’s checksum (SHA-256) is not published; it cannot be downloaded safely.',
      hash_mismatch: 'The downloaded file’s checksum does not match; the file was deleted.',
      unsigned: 'The installer package is not signed or its signature could not be verified; it will not be installed.',
      untrusted_signer: 'The installer package is signed by an unknown publisher; it will not be installed.',
      // Release signature of SHA256SUMS.txt (minisign), not the installer's Authenticode.
      sums_unsigned:
        'The release signature (SHA256SUMS.txt.minisig) is missing or could not be verified; no update is offered.',
      sums_untrusted:
        'The release signature does not belong to a release key this build knows, or is invalid; no update is offered.',
      sums_mismatch: 'The release signature does not belong to this version; no update is offered.',
      sums_rollback:
        'The signature of the offered version is older than a signature this PC accepted before; no update is offered.',
      no_release_key: 'This build has no release signing key; the update check is disabled.',
      too_large: 'The installer package is larger than expected; not downloaded.',
      launch: 'The installation could not start.',
      install_failed:
        'The installation could not finish (another installation may be running). Details: %ProgramData%\\Pulsar Seat\\logs\\update-msi.log. The downloaded package is kept; you can try again.',
      install_timeout:
        'The installation did not finish within an hour. Details: %ProgramData%\\Pulsar Seat\\logs\\update-msi.log. You can try again.',
      busy: 'Another update step is running.',
      not_ready: 'Check for and download the new version first.',
      not_confirmed: 'The installation was not confirmed.',
      unsupported: 'The update check is not available in this version.'
    } as Record<string, string>
  },

  phases: {
    queued: 'queued',
    waiting_for_host: 'waiting for the host',
    binding_display: 'binding the monitor',
    launching_host: 'starting the seat host',
    connecting: 'connecting',
    waiting_for_session: 'waiting for the session',
    logging_on: 'signing in'
  } as Record<string, string>,
  compat: {
    title: 'Compatibility',
    hint: 'Optional, visible system changes for seat PCs. Each one is off by default, recorded and undone when you turn it off, turn Pulsar Seat off or uninstall it.',
    fastStartup: 'Turn off fast startup (restart needed)',
    fastStartupHint: 'With fast startup, “Shut down” keeps the Windows kernel and drivers in a file; seats then come back half-initialised. Off means every start is a clean start.',
    wu: 'Windows Update for seat PCs',
    wuHint: 'Writes the documented Windows Update for Business policies. Updates are delayed, not blocked; security updates still arrive after the delay. Windows Home ignores these policies.',
    wuEnabled: 'Use these Windows Update settings',
    wuFeature: 'Delay feature updates by (days, 0–365)',
    wuQuality: 'Delay quality updates by (days, 0–30)',
    wuActiveHours: 'Active hours (no automatic restart in this window)',
    wuActiveFrom: 'From',
    wuActiveTo: 'to',
    wuActiveNone: 'Windows decides',
    wuNoRestart: 'Never restart automatically while someone is signed in',
    wuInvalid: 'Check the values: up to 365 / 30 days, active hours both set, at most 18 hours.',
    deny: 'Block new devices of unused kinds',
    denyHint: 'Device Installation Restrictions policy: Windows does not install new devices of the ticked kinds. Devices already installed keep working. Useful against a seat user plugging in devices no seat should get.',
    denyUnused: (list: string) => `No seat uses: ${list}.`,
    denyPickUnused: 'Tick the unused kinds',
    denyAdmins: 'Administrators may still install these devices',
    installClasses: {
      printer: 'Printers',
      image: 'Scanners and imaging devices',
      camera: 'Cameras',
      wpd: 'Phones and portable devices',
      modem: 'Modems',
      cdrom: 'CD / DVD drives',
      ports: 'Serial and parallel ports',
      smart_card_reader: 'Smart-card readers'
    } as Record<string, string>,
    defender: 'Exclude Pulsar Seat from Microsoft Defender scans',
    defenderHint:
      'Adds Pulsar Seat’s install folder, its data folder and its three background programs to the Defender exclusion lists, so scans do not slow seat start-up. Exclusions you made yourself are left alone. Nothing happens when another antivirus is active.',
    apps: 'Apps on seats',
    steamPerSeat: 'Steam on every seat (own Steam client per local seat)',
    steamPerSeatHint:
      'Steam started in a seat’s own session gets that seat’s own IPC name, so each seat runs its own Steam client. Needs per-process patching with steam.exe on the target list. Each seat should use its own Steam account (Steam’s rules). The game library folder stays shared: two seats installing or updating the same game at once can collide; give a seat its own Steam install folder for a separate library.',
    objectRules: 'Single-instance programs (one copy per seat)',
    objectRulesHint:
      'For programs that refuse a second copy on the PC: the named objects listed here get a per-seat name inside the program. Only programs on the per-process patching target list are covered; never add games with anti-cheat. Object names are case-sensitive; end a name with * to match a prefix.',
    objectExe: 'Program (file name, e.g. launcher.exe)',
    objectNames: 'Object names (one per line, without Global\\)',
    objectAdd: 'Add program',
    objectRemove: 'Remove',
    objectInvalid: 'A rule is incomplete: a .exe file name and at least one name are needed.',
    steamProxy: 'Old Steam add-on DLL',
    steamProxyHint:
      'Old “multibox” add-ons put a version.dll next to steam.exe, which Steam then loads. Pulsar Seat never deletes it; you can move it into Pulsar Seat’s quarantine folder and back.',
    steamProxyCheck: 'Look for it',
    steamProxyNone: 'No add-on DLL found in the Steam folders.',
    steamProxyNoSteam: 'Steam is not installed.',
    steamProxyQuarantine: 'Move to quarantine',
    steamProxyRestore: 'Put back',
    steamProxyQuarantined: (orig: string) => `In quarantine (was ${orig})`,
    usbWizard: 'USB dock wizard'
  },

  seatCompat: {
    menu: 'Environment and sign-in…',
    title: (n: number) => `Seat ${n}: environment and sign-in`,
    env: 'Environment variables',
    envHint:
      'Set for this seat’s user at every sign-in (and on programs Pulsar Seat starts in the seat). Undone when Pulsar Seat is turned off. Windows’ own variables (Path, TEMP, …) cannot be changed here.',
    envName: 'Name',
    envValue: 'Value',
    envAdd: 'Add variable',
    envRemove: 'Remove',
    envInvalid: 'Check the names: letters, digits and _ - . ( ) only, not a Windows variable, each name once, at most 32.',
    deny: 'Deny log on locally for this seat’s account',
    denyHint:
      'The seat’s user cannot sign in at the PC itself (console or a physical seat) while the seat signs in through Remote Desktop. Pulsar Seat adds the Windows right “Deny log on locally” to the account and removes it again when turned off.',
    denyUnavailable: 'Only for seats with an account that sign in through Remote Desktop (remote seats, or seats 2 and up with the Remote Desktop session method).'
  },

  usbDock: {
    title: 'USB dock wizard',
    intro:
      'Multi-seat USB docks bring a monitor, sound and USB ports for keyboard and mouse in one box. Plug in every dock, extend the desktop onto their monitors in Display settings, then pick a seat for each dock here.',
    scanning: 'Looking for docks…',
    found: (n: number) => `${n} dock(s) found.`,
    stable: 'The count is stable.',
    unstable: 'The count is still changing; wait until every dock is ready.',
    rescan: 'Look again',
    display: 'Display settings',
    none: 'No dock with a monitor and keyboard, mouse or sound was found.',
    dock: (label: string) => `Dock ${label}`,
    contents: (m: number, i: number, a: number) => `${m} monitor(s), ${i} input device(s), ${a} sound device(s)`,
    seat: 'Seat',
    newSeat: 'New seat',
    skip: 'Leave as it is',
    hasDisplay: 'already has a monitor',
    apply: 'Assign',
    done: (n: number) => `${n} dock(s) assigned. Save to apply.`
  }
};

const productText = {
  disabled: 'Off',
  enabled: 'On',
  safe_mode: 'Safe mode (seat 1 only)',
  enabledStarted: 'On, seats running',
  enabledNotStarted: 'On, seats not started'
} as Record<string, string>;

const safeModeText: Record<string, string> = {
  windows_safe_mode: 'Windows started in safe mode; Pulsar Seat is not running. Restart the PC normally.',
  unclean_shutdown: 'Windows did not shut down cleanly last time (crash or power cut). To avoid a loop, extra seats were not started. Fix the cause and restart; Pulsar Seat then starts normally.',
  crash_loop: 'The Pulsar Seat service failed to start several times in a row and stayed in safe mode. Create a diagnostic report and update Pulsar Seat.'
};

const failureText: Record<string, string> = {
  no_monitor: 'The wizard needs at least one monitor',
  resolution_too_low: 'The main monitor’s resolution is too low for the wizard',
  unknown_monitor: 'This monitor is not among the wizard’s monitors',
  edition_unsupported: 'This Windows edition does not support this session method',
  rds_not_ready: 'Remote Desktop Session Host is not ready',
  seat_limit: 'This many seats cannot run on this PC',
  account: 'Account not found or disabled',
  logon_failed: 'Sign-in failed (wrong or expired password)',
  client_auth: 'Connection authentication refused',
  host_exited: 'The seat host closed again and again',
  session_timeout: 'The session did not start in time',
  display: 'No monitor could be bound to the seat',
  driver: 'A needed Pulsar driver is not loaded or not signed; update Pulsar Seat',
  coexistence: 'Another conflicting multiseat product was found',
  same_account_denied: 'This account is already signed in on another seat',
  policy: 'A Windows policy blocks this action',
  internal: 'An unexpected error occurred',
  console_in_use: 'An unlocked session is on the console; lock the console first (Win+L)',
  console_busy: 'Another seat is on the console now; lock that seat first',
  session_not_locked: 'The seat’s session is not locked and no password is saved; lock the session or save the password',
  nla_required:
    'The separate port for seats without a password is off; turn it on in the Advanced tab or save a password for the seat',
  loopback_listener_unavailable: 'The separate port for seats without a password is not ready',
  kiosk_app_missing: 'No program is chosen for the kiosk seat',
  console_not_ready: 'The console session is not ready yet; try again once Windows has started',
  not_our_session: 'The session found does not belong to this seat; restart the seat',
  fsu_disabled: 'Fast user switching is turned off by a policy; this method does not work until the policy is removed',
  no_account: 'No account is chosen for the seat',
  account_unresolved: 'The seat’s account was not found on this PC',

  elevation_cancelled: 'Administrator permission was not given',
  pipe_server_untrusted: 'A program other than the Pulsar Seat service opened the control channel; no connection was made',
  shell_failed: 'The window could not open'
};

const errorText: Record<string, string> = {
  service_unavailable: t.serviceDown,
  access_denied: t.adminNeeded,
  admin_only: 'An administrator set the settings so that only administrators can change them.',
  version_mismatch: 'The manager and service versions do not match. Reinstall Pulsar Seat.',
  disabled: 'Pulsar Seat is off. Turn it on first.',
  safe_mode: 'Pulsar Seat is in safe mode; extra seats do not start.',
  revision_conflict: 'The settings changed elsewhere in the meantime. Reload and try again.',
  conflicts_not_confirmed: 'Some assignments cannot be applied; not saved without confirmation.',
  invalid_config: 'The settings have errors; not saved.',
  invalid_value: 'Invalid value.',
  not_found: 'Not found.',
  seat_running: 'The seat is already running.',
  seat_not_running: 'The seat is not running.',
  unsupported: 'The selected method does not support this.',
  not_ready: 'Not ready yet; try again in a moment.',
  timeout: 'Timed out.',
  driver_unavailable: 'A needed Pulsar driver is not usable.',
  coexistence: 'Another conflicting multiseat product was found.',
  policy_blocked: 'A Windows policy blocks this action.',
  logon_failed: 'Sign-in failed.',
  warning_not_acknowledged: 'The experimental method needs the warning to be acknowledged.',
  wizard_state: 'The wizard does not accept this action right now.',
  os: 'A Windows operation failed.',
  store: 'The settings file could not be written.',
  not_implemented: 'This action is not in this version of the service yet.',
  wrong_seat_kind: 'This action cannot be done on this kind of seat (local / remote seat).',
  session_unavailable: 'Remote seats need several sessions at the same time; the selected session method does not provide that.',
  bad_request: 'Invalid request.',
  internal: 'An unexpected error occurred.'
};

const issueText: Record<string, string> = {
  too_many_seats: 'There can be at most 16 seats.',
  duplicate_seat: 'The same seat number appears twice.',
  invalid_seat_name: 'Invalid seat name (1–64 characters).',
  invalid_account: 'Invalid sign-in account.',
  seat_one_no_display: 'Seat 1 must have a monitor; cannot save.',
  exclusive_on_several_seats: 'This device can be on only one seat.',
  mask_beyond_seats: 'The device is assigned to a seat that does not exist.',
  duplicate_device: 'The same device is assigned twice.',
  timeout_out_of_range: 'A timing value is outside the allowed range.',
  loopback_port_invalid: 'The port for seats without a password must be from 1024 to 65535 and cannot be 3389.',
  hide_fsu_with_fast_user_switch: 'Hiding fast user switching turns off the “one seat at a time” session method.',
  loopback_no_accounts:
    'This seat opens at the sign-in screen, but no account can use the port for seats without a password; the seat cannot start. Add the people who will sign in on this seat to the “extra accounts” list in the Advanced tab, or give the seat an account.',
  logon_screen_needs_listener:
    'This seat opens at the sign-in screen; it cannot start while the port for seats without a password is off. Turn the port on, or give the seat an account with a password.',
  loopback_account_invalid: 'One of the extra accounts is invalid (write user or DOMAIN\\user, do not use @); it is ignored.',
  invalid_hotkey: 'The switch shortcut is invalid.',
  experimental_not_acknowledged: 'The warning for the experimental session method was not acknowledged.',
  seat_without_display: 'The seat has no monitor; it waits until one is plugged in.',
  seat_without_keyboard: 'The seat has no keyboard.',
  seat_without_mouse: 'The seat has no mouse.',
  child_of_other_seat_hub: 'The device is plugged into a USB hub given to another seat.',
  same_account_on_seats: 'The same account is on several seats and that is not allowed.',
  key_collision: 'Two devices cannot be told apart; plug them into different ports.',
  monitor_hub_mismatch: 'The monitor’s own USB hub is given to a seat, but the monitor is not on that seat.',
  cpu_unknown_id: 'Some of the chosen cores do not exist on this PC; they are ignored.',
  cpu_no_core: 'The seat is left with no core; cannot save.',
  cpu_only_efficiency_cores: 'The seat got only efficiency cores; it may run slowly.',
  invalid_ip_address: 'Invalid IP address.',
  invalid_pad_ignore_device: 'The list of pads to ignore has an entry not in 0xVVVV/0xPPPP form (or the list is too long).',
  invalid_seat_environment: 'A seat environment variable is invalid (name, value, a duplicate, or more than 32).',
  deny_local_logon_unsupported: '“Deny log on locally” works only for seats with an account that sign in through Remote Desktop; the seat would lock its own user out.',
  invalid_windows_update_policy: 'The Windows Update settings are out of range.',
  invalid_object_rule: 'A single-instance program rule is incomplete or invalid.',
  duplicate_ip_address: 'The same IP address is given to several seats.',
  too_many_ip_programs: 'The program list is too long; the rest is not saved.',
  invalid_ip_program: 'The program list has a row with an empty path.',
  loopback_ip_address: 'Addresses starting with 127 only work inside this PC; the seat cannot reach the LAN with this address.',
  link_local_ip_address: 'Windows assigns addresses starting with 169.254 itself and they can change; choose a static address.',
  ip_binding_shared_session: 'This kiosk seat shares its session with other seats; the IP address cannot be enforced by the driver, only programs that support it use it.',
  clone_not_allowed: 'Several outputs are chosen for a monitor but cloning is not allowed.',
  same_account_fast_user_switch: 'With fast user switching, each seat must have its own account.',
  invalid_gpu_preference: 'The programs in the graphics card preference are invalid (at most 64 programs, each a full .exe path).',
  invalid_kiosk_shell: 'The kiosk seat settings are invalid (at most 16 apps, name and path filled, idle reset 0–1440 minutes).',
  kiosk_shell_ignored_on_seat_one: 'Seat 1 stays a normal desktop, so the kiosk setting is not used on this seat.',
  seat_kind_mismatch: 'The seat’s kind (local / remote) does not match its settings.',
  console_seat_remote: 'Seat 1 is always a local seat.',
  device_on_remote_seat: 'Devices cannot be assigned to a remote seat.',
  remote_seat_invalid: 'The remote seat’s settings have errors.',
  session_unavailable: 'Remote seats cannot start with the selected session method (it needs several sessions at the same time).',

  store_failed: 'The settings could not be written to the file.'
};

const noticeText: Record<string, string> = {
  gpu_preference_unavailable:
    'The seat’s graphics card preference could not be applied: the PC has only one graphics card, or Windows does not see the seat’s graphics card as high-performance or power-saving.',
  os_build_changed: 'Windows was updated. Review the seat settings and save them again if needed.',
  service_gap_no_input: 'The service did not respond for a short time; input may have stopped for a moment.',
  same_account_warning: 'The same account is signed in on several seats. This is not recommended.',
  seat_starting: 'The seat is starting.',
  run_entry_missing: 'The manager startup entry needed by the chosen start mode is missing; seats may not start by themselves.',
  program_files_damaged: 'Pulsar Seat files are missing or damaged. Please reinstall.',
  hardware_changed: 'The plugged-in devices differ from the saved configuration.',
  devices_beyond_limit: 'Devices above the seat limit were made spare. Remember to save.',
  reboot_needed: 'A restart is needed to finish the change.',
  seat_start_failed: 'A seat could not start.',
  password_failing: 'A seat’s saved password was rejected; enter the password again.',
  tweak_failed: 'An optional system setting could not be applied or undone; details in the service log.',
  keep_awake_failed: 'The PC could not be kept awake while extra seats are in use.',
  input_move_pending: 'Keyboards and mice moved with the shortcut are waiting for confirmation.',
  credprov_failed: 'The seat sign-in component could not be registered or removed; details in the service log.',
  credprov_logon_failed: 'A seat’s automatic sign-in failed with the saved password; enter the password again.',
  switch_refused: 'Could not switch to the seat; details in the service log.',
  dpi_mismatch: 'The seat’s display scale differs from its setting (Windows chose another scale).'
};

/** Text per state / reason of the passwordless-seat listener (`loopback_listener:<state>[:<x>]`). */
const loopbackStateText: Record<string, string> = {
  unknown: 'not checked yet',
  disabled: 'turned off in the settings',
  not_needed: 'not needed: every seat has a saved password',
  ready: 'ready',
  pending_restart:
    'waiting for Remote Desktop Services to restart; this happens by itself once the open Remote Desktop seats close',
  port_invalid: 'invalid port (must be 1024–65535 and cannot be Remote Desktop’s own port)',
  port_in_use: 'the port is used by another program or another Remote Desktop listener',
  firewall_off: 'Windows Firewall is off in at least one network profile; turn it on',
  firewall_local_rules_ignored:
    'a group policy makes the firewall ignore local rules; the protection rule cannot apply',
  nla_forced_by_policy: 'a policy requires network level authentication on all connections',
  tls_off_by_policy: 'a policy turns off encrypted (TLS) connections',
  no_accounts: 'no account can use this port, or the accounts were not found',
  rdp_tcp_missing: 'the Remote Desktop listener (RDP-Tcp) was not found',
  write_failed: 'the settings could not be written to the registry',
  termservice_restart_failed: 'Remote Desktop Services could not restart',
  restart_needs_reboot: 'Remote Desktop Services cannot stop on this Windows version; restart the PC',
  not_listening: 'the port is not listening yet; wait a bit and try again, or restart the PC',
  untrusted_listener: 'a program other than Remote Desktop Services listens on the port',
  key_changed: 'the listener settings were changed afterwards; fixed when the seat restarts',
  firewall_rule_missing: 'the protecting firewall rule is missing or was changed'
};

/** Strings used by the label helpers in i18n.ts. */
const misc = {
  errorPrefix: (what: string) => `Error: ${what}`,
  issueSeat: (n: number) => `seat ${n}`,
  listAnd: ' and ',
  loopbackHead: 'Port for seats without a password',
  loopbackReady: (port: string) => `ready (${port})`,
  loopbackExposed: (port: string) =>
    `WARNING: the removed port for seats without a password (${port}) still runs without network level authentication and the firewall does not protect it. Close the Remote Desktop seats or restart the PC`,
  loopbackUnloadPending: (port: string) =>
    `The removed port for seats without a password (${port}) runs until Remote Desktop Services restart; it is removed once the open Remote Desktop seats close`
};

// ---------------------------------------------------------------- remote seats
// Copy of the remote seat pages (from Pulsar Split's manager, merge-plan.md step 9). Kept as
// its own object so the remote pages keep their keys.

const rt = {
  appTitle: 'Pulsar Seat Manager',
  nav: {
    newSeat: 'New remote seat',
    startService: 'Start the service',
    restartService: 'Restart the service',
    stopService: 'Stop the service',
    settings: 'Settings',
    host: 'Host PC',
    logs: 'Logs',
    dxvk: 'DXVK games',
    vm: 'VM seats',
    noSeats: 'No remote seat yet',
    session: (id: number) => `Session ${id}`,
    streaming: 'streaming',
    streamingTo: (client: string) => `streaming: ${client}`,
    sessionUnavailable: 'Cannot start: no concurrent sessions',
    sessionUnavailableHint:
      'Remote seats need several Windows sessions at the same time. The session method running on this PC does not provide that (for example a client edition of Windows or fast user switching). The seat cannot start.'
  },
  /** Remote seats root page and sub-pages (redesign 2026-11-17). */
  home: {
    intro: 'Seats used over the network from a TV, a phone or another computer, with Pulsar or Moonlight.',
    thisPc: 'This PC',
    thisPcHelp: 'Shared by all remote seats.',
    hostHelp: 'Windows edition, multiseat readiness, drivers',
    settings: 'Remote seat settings',
    settingsHelp: 'Isolation, gamepads, virtual display',
    dxvkHelp: 'Run old Direct3D games through Vulkan',
    logsHelp: 'Service and seat logs',
    vmHelp: 'Seats that run in their own Hyper-V virtual machine',
    seatNumber: (n: number) => `Seat ${n}`,
    streamWith: (backend: string) => `streaming with ${backend}`,
    pairedCount: (n: number) => (n === 1 ? '1 paired device' : `${n} paired devices`)
  },
  seatView: {
    locked: 'Settings cannot change while the seat runs. Stop the seat to change them.',
    runtime: 'Runtime state',
    removal: 'Removal',
    passwordSaved: 'Saved',
    test: 'Test',
    testLabel: 'Test the password',
    change: 'Change',
    changeLabel: 'Change the password',
    regenerate: 'Renew identity',
    regenerateRow: 'Renew the seat identity',
    copyAddress: 'Copy the address',
    portRange: (from: number, to: number) => `Range in use: ${from}–${to}.`
  },
  vm: {
    title: 'VM seats',
    intro:
      'A VM seat runs its Windows session in a Hyper-V virtual machine of its own: one PC, no shared session, streamed like the other remote seats. Each seat’s VM starts from a common base disk and keeps its own changes.',
    host: 'Hyper-V host',
    refresh: 'Refresh',
    feature: (s: string) => `Hyper-V: ${s}`,
    featureStates: { enabled: 'on', disabled: 'off', enable_pending: 'on after a restart', unavailable: 'not available on this Windows edition' } as Record<string, string>,
    enableHint: 'Turn Hyper-V on in the Network tab first (a restart follows).',
    hostFacts: (mb: number, cpus: number) => `${Math.round(mb / 1024)} GB memory, ${cpus} logical processors`,
    gpus: (n: number) => (n === 0 ? 'No GPU can be partitioned: VM seats render in software.' : `${n} GPU(s) can be partitioned.`),
    network: 'VM network',
    networkReady: 'Ready.',
    networkMissing: 'Not set up yet.',
    prepareNetwork: 'Set up the VM network',
    confirmNetwork: 'Create an internal virtual switch and a NAT for the VM seats? Uninstall removes them.',
    base: 'Base disk',
    basePresent: 'Built.',
    baseMissing: 'Not built yet.',
    building: 'Building the base disk… this takes several minutes.',
    buildBase: 'Build the base disk',
    confirmBuild: 'Build the base disk from the chosen Windows image now? It takes several minutes and needs free disk space.',
    resetBase: 'Delete the base disk',
    confirmResetBase: 'Delete the base disk and every VM seat’s disk? All changes inside the VM seats are lost.',
    lastError: (e: string) => `Last error: ${e}`,
    settings: 'Settings',
    storage: 'Storage folder (empty = default)',
    memoryPercent: 'Memory for all VM seats (% of this PC)',
    storageGb: 'Base disk size (GB)',
    imageKind: 'Windows image',
    imageKinds: { none: 'none chosen', wim: 'setup image (.wim / .esd)', vhdx: 'prepared disk (.vhdx)' } as Record<string, string>,
    imagePath: 'Image path',
    imageIndex: 'Edition index',
    languageCab: 'Language pack (.cab, optional)',
    restartGpus: 'Restart the graphics adapters after a VM with a GPU stops',
    natPrefix: 'VM network (a.b.c.0/len)',
    saveSettings: 'Save settings',
    seats: 'Seats',
    noRemoteSeats: 'No remote seat yet. Create one, then turn VM mode on here.',
    vmMode: 'Run this seat in a VM',
    memory: 'Memory (MB, 0 = automatic)',
    processors: 'Processors (0 = automatic)',
    gpu: 'Graphics',
    gpuModes: { off: 'none (software)', auto: 'partition of the first GPU', adapter: 'partition of this GPU' } as Record<string, string>,
    gpuAdapter: 'GPU',
    stop: 'When the seat stops',
    stopModes: { shut_down: 'shut the VM down', save: 'save the VM state', turn_off: 'turn the VM off' } as Record<string, string>,
    hideSuffix: 'Advertise the name without “(VM)”',
    guestAddress: (a: string) => (a ? `VM address: ${a}` : 'VM address: assigned when saved'),
    state: (s: string) => `VM: ${s}`,
    guestPhases: { waiting: 'waiting for its settings', configuring: 'setting up', signed_in: 'signed in', streaming: 'streaming', failed: 'failed' } as Record<string, string>,
    shared: 'Shared folders',
    addFolder: 'Add folder',
    hostFolder: 'Folder on this PC',
    mountPoint: 'Path in the VM',
    readOnly: 'read only',
    remove: 'Remove',
    forwards: 'Forwarded ports (host to VM)',
    forwardsHint: 'The seat’s streaming ports are always forwarded.',
    addForward: 'Add port',
    description: 'Description',
    protocol: 'Protocol',
    hostPort: 'Host port',
    guestPort: 'VM port',
    saveSeat: 'Save seat',
    resetSandbox: 'Reset the seat’s disk',
    confirmResetSandbox: 'Reset this seat’s disk? Everything changed inside its VM is lost.',
    saved: 'Saved.',
    problems: {
      memory: 'Memory must be 0 (automatic) or at least 2048 MB.',
      gpuAdapter: 'Choose the GPU.',
      saveWithGpu: 'A VM with a GPU cannot be saved; shut it down instead.',
      hostPath: 'The folder must be an absolute path on this PC.',
      mountPoint: 'Use a drive letter other than C:, or a C:\\ path with a folder and no trailing backslash.',
      mountTwice: 'The same path in the VM is used twice.',
      port: 'Ports must be 1-65535.',
      portInStream: 'The host port is one of the seat’s streaming ports.',
      portTwice: 'The same host port is forwarded twice.',
      storage: 'The storage folder must be an absolute path.',
      memoryPercent: 'Memory share must be 10-90 %.',
      storageGb: 'Size must be 32-4096 GB.',
      image: 'Choose an absolute path to a .wim / .esd or .vhdx file.',
      languageCab: 'The language pack must be an absolute .cab path.',
      natPrefix: 'Use a network such as 172.31.240.0/24 (length 16-28).'
    } as Record<string, string>
  },
  dxvk: {
    title: 'DXVK games',
    intro:
      'DXVK runs old Direct3D 8/9 games through Vulkan. Files are copied only into the game’s folder; turning it off brings the old files back.',
    release: (v: string) => `Downloaded DXVK version: ${v}`,
    noRelease: 'DXVK is not downloaded yet. First run "pulsar-split-shimctl dxvk download" in an administrator command prompt.',
    games: 'Games',
    noGames: 'No game has DXVK turned on yet.',
    exe: 'Game file (.exe)',
    browse: 'Browse',
    pickTitle: 'Choose the game’s .exe file',
    dlls: 'Direct3D versions to redirect',
    enable: 'Turn DXVK on',
    disable: 'Turn DXVK off',
    deployed: (v: string | null, dlls: string) => `On (${dlls}${v ? `, version ${v}` : ''})`,
    notDeployed: 'Off (files removed)',
    markerUnreadable: 'The marker file could not be read; check the files by hand.',
    enabled: 'DXVK turned on.',
    disabled: 'DXVK turned off.',
    needDll: 'Choose at least one Direct3D version.',
    needExe: 'Enter the full .exe path of the game.',
    refresh: 'Refresh'
  },
  logs: {
    title: 'Logs',
    source: 'Source',
    service: 'Service',
    seat: 'Seat',
    refresh: 'Refresh',
    redacted: 'Lines that may contain a password, PIN or key are hidden.',
    empty: 'The log has no lines.'
  },
  service: {
    title: 'Service',
    reachable: 'Connected to the service',
    unreachable: 'The service cannot be reached',
    consoleMode: 'developer (console) mode',
    version: 'Version',
    starting: 'Starting the service…',
    stopping: 'Stopping the service…',
    restarting: 'Restarting the service…',
    pendingHint: 'The service is changing state right now; wait a moment.'
  },
  serviceState: {
    not_installed: 'Not installed',
    stopped: 'Stopped',
    running: 'Running',
    pending: 'Changing',
    unknown: 'Unknown'
  } satisfies Record<ServiceRunState, string>,
  settings: {
    title: 'Settings',
    windows: 'Windows',
    startWithWindows: 'Start with Windows',
    isolation: 'Isolation',
    hidIsolation: 'Isolate HID devices',
    processPatching: 'Turn on process patching',
    steamIsolation: 'Split Steam per seat',
    isolationNote:
      'HID isolation and the Steam split apply to newly started seats. Process patching loads the Pulsar Seat module into the apps you choose (x64 and ARM64 PCs). On an ARM64 PC, x64 apps get the module only when Pulsar Seat starts them (stream apps, kiosk apps); an x64 app you start yourself is not patched and is left out of the list Windows uses.',
    antiCheat:
      'Warning: some games that use anti-cheat software may see the module loaded into processes, or remote session masking, as a problem.',
    isolationState: 'Isolation state',
    installed: 'Module installed',
    notInstalled: 'Module not installed (device security only)',
    unsupportedArch: 'No process patching module for this architecture is installed on this PC.',
    killSwitch: 'Kill switch on: the module installs no hook in any process.',
    foreign: (s: string) => `Another product’s patching database was found (${s}). Turn off that product’s process patching first.`,
    targets: 'Apps the module loads into (file name)',
    targetsGameInput:
      'Adding GameInputSvc.exe (the Windows game-input service) turns on a controller-focus filter there: a remote seat’s window coming to the front no longer takes controller focus from this PC. Experimental, off unless listed.',
    addTarget: 'Add',
    maskApps: 'Apps the remote session is hidden from (full path)',
    maskHint: 'On the seat, the app does not see that it runs in a remote session. Turn it on only for games that need it.',
    pads: 'Gamepads',
    padBackend: 'Virtual gamepad driver',
    auto: 'Automatic',
    bluetoothConsoleOnly: 'Use Bluetooth only on the physical screen (console session)',
    padAudio: 'DualSense speaker, microphone and haptics audio also go to remote seats (Pulsar streaming only; applies after the service restarts)',
    displayBackend: 'Virtual display method',
    log: 'Logging',
    verbosity: 'Detail level'
  },
  verbosity: {
    none: 'None',
    error: 'Error',
    warning: 'Warning',
    info: 'Information',
    debug: 'Debug'
  } satisfies Record<Verbosity, string>,
  seat: {
    actions: 'Actions',
    start: 'Start seat',
    stop: 'Stop seat',
    delete: 'Delete seat',
    pair: 'Pair a new device',
    general: 'Seat settings',
    displayName: 'Display name',
    port: 'Port',
    portHint: (p: number) => `Port range in use: ${p - 5}–${p + 21}`,
    startWithService: 'Start with the service',
    stopAction: 'Session when stopping',
    stopLogOff: 'Sign out',
    stopDisconnect: 'Disconnect, keep the session open',
    user: 'User',
    userName: 'Windows user',
    chooseUser: 'Choose a user',
    password: 'Password',
    passwordSet: 'Password saved',
    passwordMissing: 'No password entered',
    savePassword: 'Save password',
    passwordSaved: 'Password saved.',
    testCredentials: 'Test the user credentials',
    show: 'Show',
    hide: 'Hide',
    display: 'Display',
    renderAdapter: 'Graphics card (empty: best fit)',
    scale: 'Desktop scale',
    scaleDynamic: 'Dynamic (follow the client)',
    scaleStatic: 'Fixed',
    superSampling: 'Supersampling',
    sdrWhite: 'SDR white level',
    forceSdr: 'Force SDR on HDR streams',
    autostart: 'Apps that start automatically',
    autostartPath: 'App path (.exe)',
    autostartArgs: 'Parameters',
    autostartAsSystem: 'Run as SYSTEM',
    addApp: 'Add app',
    remove: 'Remove',
    lockedHint: 'Settings cannot change while the seat is running.',
    saved: 'Saved',
    crashes: (n: number) => (n === 1 ? 'The stream server crashed once' : `The stream server crashed ${n} times`),
    lastError: 'Last error',
    failure: 'Reason for failure',
    phase: 'Step',
    runtime: 'Runtime state',
    session: (id: number) => `Windows session: ${id}`,
    clientName: 'RDP client name',
    displayBackend: 'Display',
    hdr: 'HDR',
    hdrOk: 'Available',
    audio: 'Sound',
    pads: 'Gamepads',
    noPads: 'No virtual gamepad connected',
    stream: 'Stream server',
    streamRunning: 'Running',
    streamActive: 'Streaming',
    isolationHealth: 'Isolation module',
    adapterAuto: 'Best-fit graphics card',
    refresh: 'Refresh rate (Hz)',
    luminance: 'HDR brightness (advanced)',
    maxNits: 'Peak (nits)',
    maxFullFrame: 'Full-frame peak (nits)',
    fixedResolution: 'Stream resolution',
    fixedFollow: 'Follow the client',
    fixedCustom: 'Fixed (custom)',
    fixedWidth: 'Width (px)',
    fixedHeight: 'Height (px)',
    lockRefresh: 'Keep this refresh rate whatever the client asks for',
    modeControl: 'Display mode now',
    modeWidth: 'Width',
    modeHeight: 'Height',
    modeHz: 'Hz',
    modeHdr: 'HDR',
    modeApply: 'Apply',
    modeHint:
      'Changes the running seat’s display at once, without a restart. The seat’s fixed resolution and locked refresh rate still apply; the client may ask for another mode later.',
    modeApplied: (w: number, h: number, hz: number, rw: number, rh: number) =>
      `Applied: ${w} × ${h} @ ${hz} Hz (rendered ${rw} × ${rh})`,
    displayHost: 'Session path',
    hostProvider: 'Pulsar protocol provider: own display with mode and HDR control',
    hostLoopback: 'Remote Desktop client: stock display, mode follows the client',
    fallback: 'Own display not used',
    audioSection: 'Sound',
    streamAudio: 'Add the seat’s sound to the stream',
    microphone: 'Microphone (Pulsar client only)',
    microphoneHint:
      'The Pulsar client’s microphone is given to the apps on the seat as a microphone. A pulsar-host that supports the microphone is needed; Moonlight does not carry a microphone. The change applies when the seat restarts.',
    mic: 'Microphone',
    micState: {
      off: 'Off',
      unsupported: 'This pulsar-host version does not support the microphone',
      waiting: 'Waiting for a client connection',
      connected: 'Connected',
      failed: 'The microphone channel could not open',
      closed: 'Not connected'
    } as Record<string, string>,
    browse: 'Browse',
    pickAutostart: 'Choose the program to start',
    gpuNoHardware: 'No hardware graphics card found: video is encoded on the processor, the stream may be slow.',
    gpuNoEncoder: (name: string) => `No hardware video encoder is known for ${name}; encoding may fall back to the processor.`,
    gpuHybrid:
      'There are several graphics cards (hybrid system). Choose the card with the encoder here by hand; automatic choice takes the one with the most memory.',
    streamSection: 'Stream',
    gamepad: 'Gamepad type',
    gamepadAuto: 'Automatic',
    gamepadPulsar: (kind: string) => `${kind} (Pulsar pad, isolated)`,
    capturePulsarIdd: 'Pulsar virtual display (HDR)',
    pulsarBuildOnly: 'Needs the Pulsar build of the stream server; with the standard build the server’s default is used.',
    capture: 'Screen capture',
    captureDefault: 'Default',
    minFps: 'Lowest FPS target',
    firewallDomain: 'Also open to domain networks in the firewall',
    keepPairing: 'Keep the pairings when the seat is deleted',
    apps: 'App list (shown on the client)',
    appName: 'Name',
    appCmd: 'Command',
    saveApps: 'Save the list',
    appsSaved: 'App list saved.',
    identity: 'Seat identity',
    regenerate: 'Renew the identity (all pairings are deleted)',
    regenerateConfirm: 'The seat’s certificate is renewed and every paired device must pair again.',
    regenerateConfirmPulsar:
      'The seat’s identity is renewed: the Pulsar ID changes and every paired device must pair again.',
    identityDone: 'Identity renewed.',
    streamBackend: 'Stream server',
    backendSunshine: 'Sunshine (Moonlight compatible)',
    backendPulsar: 'Pulsar',
    backendMissing: 'not installed',
    backendMissingHint: (b: string) => `${b} is not installed on this PC; the seat cannot start with this stream server.`,
    backendStopFirst: 'Stop the seat first to change the stream server.',
    relayId: 'Pulsar ID',
    relayState: (s: string) =>
      (({ ok: 'connected', auth_required: 'waiting for sign-in', unreachable: 'unreachable', version_mismatch: 'version mismatch' }) as Record<string, string>)[s] ?? s,
    pulsarOptions: 'Pulsar connection',
    pulsarRelay: 'Relay server',
    pulsarRelayHint: 'host or host:port; empty uses the default relay.',
    pulsarRelayInvalid: 'Enter a host name or address, optionally with :port.',
    pulsarNetworkMode: 'Connection type',
    pulsarNetworkAuto: 'Direct when possible, otherwise through the relay',
    pulsarNetworkP2p: 'Direct only',
    pulsarNetworkRelay: 'Relay only',
    pulsarNodePort: 'Network port (UDP)',
    pulsarNodePortBase: (p: number) => `Seat port (${p})`,
    pulsarNodePortEphemeral: 'Any free port',
    pulsarNodePortFixed: 'This port:',
    pulsarNodePortInvalid: 'The port must be 1024-65535.',
    pulsarPortMapping: 'Ask the router to open the port (UPnP)',
    pulsarDiscovery: 'Show this seat to devices on the local network',
    pulsarDiscoveryHint: 'Opens UDP 21117 for the local subnet in the firewall while the seat runs with this option.',
    pulsarAccess: 'Who can connect',
    pulsarAccessApprove: 'Every connection needs approval here',
    pulsarAccessPassword: 'Approval here, or the seat’s password',
    pulsarMaxClients: 'Devices at the same time'
  },
  seatState: {
    stopped: 'Stopped',
    booting: 'Starting',
    running: 'Running',
    stopping: 'Stopping',
    failed: 'Failed'
  } satisfies Record<RemoteSeatState, string>,
  phase: {
    user: 'Checking the user',
    session: 'Opening the session',
    display: 'Preparing the display',
    audio: 'Preparing sound',
    stream: 'Starting the stream server',
    pads: 'Preparing gamepads',
    isolation: 'Setting up isolation',
    autostart: 'Starting apps'
  } as Record<string, string>,
  host: {
    title: 'Host PC',
    audioCleanup: 'Old sound devices',
    audioCleanupHint:
      'Deletes the "Remote Audio" device entries left over from ended Remote Desktop sessions that are no longer connected. It does not touch connected devices or console devices.',
    audioCleanupRun: 'Clean up old sound devices',
    audioCleanupConfirm: 'Delete the leftover device entries permanently? Every deletion is written to the service log.',
    audioCleanupYes: 'Yes, clean up',
    audioCleanupNo: 'Cancel',
    audioCleanupDone: (removed: number, failed: number) =>
      removed === 0 && failed === 0 ? 'No old device to delete.' : `${removed} device(s) deleted, ${failed} device(s) left.`,
    edition: 'Windows edition',
    class: 'Multiseat state',
    rdsReady: 'Remote Desktop service',
    ready: 'Ready',
    notReady: 'Not ready',
    port: 'RDP port',
    maxSeats: 'Most seats at the same time',
    unlimited: 'Unlimited',
    licensing: 'RDS licensing',
    licensingMode: (m: number | null): string => (m === 2 ? 'Per device' : m === 4 ? 'Per user' : 'Not configured'),
    graceDays: (d: number) => (d === 1 ? '1 day of the grace period left' : `${d} days of the grace period left`),
    warnings: 'Warnings',
    noWarnings: 'No warnings.',
    multiSeat: 'Multiseat',
    multiSeatToggle: 'Prepare this PC for multiseat',
    multiSeatHint:
      'Changes the Remote Desktop settings for seats (the original values are kept and restored when turned off). Turning it off stops all seats.',
    passive:
      'Another multiseat product was found. Pulsar Seat is in passive mode: it changes nothing on the system and starts no seats.',
    coexistence: 'Compatibility check',
    noFindings: 'No problem found.',
    drivers: 'Drivers',
    displayDrivers: 'Virtual display',
    padDrivers: 'Virtual gamepad',
    wrds: 'Remote display protocol provider',
    wrdsReady: 'Ready: remote seats get the Pulsar display',
    wrdsNotWanted: 'Not used (display setting is not “Pulsar virtual display driver”)',
    installedYes: 'Installed',
    installedNo: 'Not installed',
    signed: 'Signed',
    unsigned: 'Unsigned',
    remoteCapable: 'Works in a remote session',
    consoleOnly: 'Physical screen only',
    hdrAvailable: 'HDR available',
    hdrUnavailable: (why: string) => `No HDR: ${why}`,
    experimental: 'Experimental: multiple sessions on client Windows',
    experimentalHint:
      'Only for Windows Pro / Enterprise, and needs an open-source wrapper that you install. Off by default.',
    twState: {
      off: 'Off',
      on: 'On',
      foreign_active: 'Another tool redirected the Remote Desktop service; Pulsar Seat does not touch it.',
      blocked: 'Turned off automatically (unsupported version or wrapper problem)'
    } as Record<string, string>,
    twEnable: 'Turn on',
    twDisable: 'Turn off',
    twNoCandidate: 'No installed wrapper was found.',
    refresh: 'Refresh'
  },
  hostClass: {
    home: 'Windows Home: multiseat is not supported.',
    client_multi_session_unavailable: 'Client edition of Windows: only one remote session at a time.',
    server_without_rdsh: 'Windows Server without the Remote Desktop Session Host role: one trial seat only.',
    server_rdsh: 'Windows Server + Remote Desktop Session Host: multiseat ready.',
    client: 'Needs the experimental multi-session feature'
  } as Record<string, string>,
  hostWarning: {
    rdsh_missing: 'The Remote Desktop Session Host role is not installed.',
    licensing_not_configured: 'RDS licensing is not configured; only one seat runs.',
    grace_ends: 'The RDS licensing grace period is about to end.',
    policy_override: 'A group policy overrides a Pulsar Seat setting.',
    seat_user_is_admin: 'The seat user is an administrator; a normal user is recommended.',
    foreign_wrapper: 'The Remote Desktop service is redirected to another wrapper.',
    autostart_run_as_system_ignored: '“Run as SYSTEM” was ignored; the app started as the seat user.',
    autostart_failed: 'An app that starts automatically could not start.',
    adapter_fallback: 'The chosen graphics card was not found; the default is used.',
    firewall: 'The firewall rules could not be added; devices on the LAN may not reach the seat.',
    control_unavailable: 'The stream server’s control channel could not open; pairing is not available on this seat.',
    capture_error: 'The stream server could not capture the screen.',
    dpi_mismatch: 'The seat’s display scale differs from its static scale setting (Windows chose another scale).',
    stream_fallback: 'This stream-server build lacks some Pulsar features; the seat uses the standard behaviour for them.',
    display_fallback: 'The Pulsar display could not be used; the seat runs on the Remote Desktop display.'
  } as Record<string, string>,
  signal: {
    ref_arp: 'Another multiseat product is installed.',
    ref_service: 'Another multiseat product has a service.',
    termsrv_wrapped: 'The Remote Desktop service is redirected.',
    bluetooth_wrapped: 'The Bluetooth service is redirected.',
    rdp_idd_redirected: 'The RDP display driver was changed.',
    ref_shim_db: 'Another product’s compatibility database is installed.',
    ref_shim_exe_map: 'Another product has a compatibility mapping.',
    ref_shim_arp: 'Another product’s patching component is installed.',
    ref_shim_file: 'Another product has a patching file.',
    compat_layer_forced: 'A compatibility layer is forced PC-wide.',
    ref_pad_driver: 'Another product’s gamepad driver is installed.',
    ref_root_cert: 'Another product’s root certificate is installed.',
    kernel_helper_residue: 'There are leftovers of an old kernel helper.',
    defender_exclusions: 'Microsoft Defender exclusions were found.',
    ref_firewall_rules: 'Another product has firewall rules.',
    rds_residue: 'There are leftovers in the Remote Desktop settings.',
    upstream_sunshine: 'A separately installed Sunshine is present (ports may clash).',
    vigem_present: 'The ViGEmBus driver is installed.'
  } as Record<string, string>,
  modeStatus: {
    ok: 'applied',
    ok_sdr_fallback: 'applied in SDR',
    clamped: 'applied with clamped values',
    no_display: 'the seat display is not ready',
    rejected: 'Windows refused the mode',
    timeout: 'the display driver did not answer in time',
    unsupported: 'this display cannot change mode (it follows the client)'
  } as Record<string, string>,
  wrdsReason: {
    wrds_unavailable: 'the protocol provider is not part of this installation',
    wrds_disabled_by_recovery: 'switched off by the recovery value WrdsDisabled',
    wrds_dll_missing: 'the provider file is missing or for another processor',
    wrds_not_registered: 'not registered yet',
    wrds_restart_needed: 'registered; restart the Remote Desktop service or the PC to load it',
    wrds_not_listening: 'loaded, but the Remote Desktop service has not started its listener',
    wrds_connect_failed: 'the session could not be created through the provider',
    wrds_timeout: 'the session through the provider did not log on in time',
    display_timeout: 'the Pulsar display did not become ready in time',
    driver_not_installed: 'the Pulsar display driver is not installed'
  } as Record<string, string>,
  hdrReason: {
    force_sdr: 'SDR is forced on the seat',
    rdp_display: 'the Remote Desktop display does not support HDR',
    os_too_old: 'the Windows version is too old',
    iddcx_too_old: 'the display driver interface is old',
    driver_no_fp16: 'the driver does not support HDR surfaces',
    gpu_unsupported: 'the graphics card does not support it',
    driver_not_installed: 'the driver is not installed',
    server_needs_whql: 'Windows Server needs a WHQL-signed driver'
  } as Record<string, string>,
  displayBackend: {
    rdp_session: 'Remote Desktop display (default)',
    vdd: 'Virtual Display Driver (signed, physical screen only)',
    pulsar_idd: 'Pulsar virtual display driver (unsigned, trial)'
  } as Record<string, string>,
  padBackend: {
    vigem: 'ViGEmBus (Xbox 360 / DualShock 4)',
    usbip_ds5: 'usbip-win2 (DualSense)',
    pulsar_pad: 'Pulsar gamepad driver (unsigned, trial)'
  } as Record<string, string>,
  audioState: {
    capturing: 'Sound is added to the stream',
    no_device: 'No sound device in the session',
    blocked_by_policy: 'A group policy blocks sound',
    disabled: 'Off',
    unknown: 'Unknown'
  } as Record<string, string>,
  audioLimitation: {
    no_microphone: 'Apps on this seat cannot see a microphone.',
    no_controller_audio: 'The gamepad speaker / headset jack is not supported.',
    steam_remote_play_unsupported: 'Steam Remote Play is not supported on seats.',
    surround_is_stereo: 'Multi-channel sound is passed as stereo.',
    policy_blocks_audio: 'A group policy blocks sound redirection.',
    mic_policy_blocked: 'A group policy blocks microphone redirection.',
    mic_needs_pulsar: 'The microphone works only with the Pulsar client; Moonlight does not send a microphone.'
  } as Record<string, string>,
  failure: {
    seat_booting: 'A seat is starting; try again when it is done.',
    dxvk_hash_mismatch: 'The DXVK files do not match the expected version; download again.',
    backend_missing: 'The chosen stream server is not installed on this PC.',
    backend_pulsar: 'This action is not available on the Pulsar stream server.',
    backend_sunshine: 'This action is not available on the Sunshine stream server.',
    edition_unsupported: 'This Windows edition does not support multiseat.',
    coexistence: 'The seat was not started because another multiseat product was found.',
    rds_not_ready: 'The Remote Desktop service is not ready.',
    seat_limit: 'The limit of seats that can run at the same time was reached.',
    account: 'The seat user cannot be used.',
    password_expired: 'The seat user’s password has expired.',
    rdu_membership: 'The user could not be added to the “Remote Desktop Users” group.',
    profile: 'The user profile could not be loaded.',
    client_auth: 'Sign-in failed: wrong user name or password.',
    cert_mismatch: 'The Remote Desktop certificate does not match the expected one.',
    session_timeout: 'The session did not open in time.',
    client_exited: 'The helper program that holds the session closed unexpectedly.',
    token: 'A secure identity for the helper program could not be created.',
    display_timeout: 'The seat’s display was not ready in time.',
    no_display: 'There is no display in the seat session.',
    driver_not_installed: 'The chosen virtual display driver is not installed.',
    session_attach_unsupported: 'This display driver does not work in remote sessions.',
    stream_server: 'The stream server crashed again and again.',
    server_binary_missing: 'The stream server is not installed.',
    server_hash_mismatch: 'The stream server file could not be verified.',
    port_in_use: 'One of the seat’s ports is in use.',
    firewall: 'The firewall rules could not be added.',
    multi_session_off: 'Turn on multiseat on the “Host PC” page first.',
    shim_foreign: 'Another product’s process patching is on.',
    shim_blocked: 'Windows application control blocked the module from loading.',
    server_without_control_pipe: 'In-app pairing is not available on this seat.',
    no_pending_request: 'There is no pending pairing request.',
    wrong_pin: 'Wrong PIN.',
    pair_timeout: 'The pairing time ran out.',
    warning_not_acknowledged: 'Read and acknowledge the warning first.',
    not_client_sku: 'This module is only for Windows Pro / Enterprise.',
    foreign_wrapper: 'The Remote Desktop service is redirected by another tool.',
    unknown_termsrv_build: 'This Windows update is not supported by the wrapper yet.',
    wrapper_missing: 'The wrapper was not found.',
    wrapper_unsigned: 'The wrapper’s signature was not accepted.',
    wrapper_insecure_acl: 'Non-administrators can write to the wrapper folder; fix the permissions.',
    wrapper_arch: 'The wrapper does not fit this PC’s architecture.',
    termservice_restart: 'The Remote Desktop service could not restart.',
    reference_product_present: 'Another multiseat product is installed.',
    dxvk_not_downloaded: 'DXVK is not downloaded yet.',
    bad_app_list: 'The app list is invalid.',
    unsupported: 'This feature is not available on this PC.',
    cancelled: 'The start was cancelled.'
  } as Record<string, string>,
  experimental: {
    title: 'Turn on the experimental feature',
    wrapper: 'Wrapper',
    ack: 'I have read and understood the risks; I accept the responsibility.',
    enable: 'Turn on and restart the Remote Desktop service',
    cancel: 'Cancel',
    working: 'Stopping the seats and restarting the service…'
  },
  newSeat: {
    title: 'New remote seat',
    name: 'Name',
    hint: 'English letters and digits only; no spaces.',
    create: 'Create',
    cancel: 'Cancel'
  },
  deleteSeat: {
    title: (n: string) => `Delete “${n}”?`,
    body: 'The seat’s settings and the files created for it are deleted.',
    warning: 'This cannot be undone.',
    confirm: 'Delete',
    cancel: 'Cancel'
  },
  pair: {
    title: (n: string) => `Pair a device with ${n}`,
    waiting: 'Waiting for a pairing request from the device…',
    pending: (d: string) => `“${d}” wants to pair. Type the PIN shown on the device.`,
    paired: 'Device paired.',
    unavailable: 'In-app pairing is not available on this seat (the stream server has no control channel).',
    notRunning: 'The seat must be running to pair.',
    cancelRequest: 'Refuse the request',
    clients: 'Paired devices',
    noClients: 'No paired device.',
    unpair: 'Remove the pairing',
    unpairAll: 'Remove all',
    step1: 'Open the Pulsar or Moonlight app on your device.',
    step2: 'Add a new host and type one of the addresses below.',
    step3: 'Choose the added PC; a four-digit PIN shows on the screen.',
    step4: 'Type the PIN here and send it.',
    addresses: 'Addresses',
    recommended: 'recommended',
    pinWhy: 'You can type the PIN once a request arrives.',
    request: (name: string) => `“${name}” wants to pair`,
    qr: (a: string) => `QR code for the address: ${a}`,
    qrHint: 'Scan it with a phone to copy the address.',
    noAddress: 'No LAN address found.',
    pin: 'PIN',
    clientName: 'Device name',
    submit: 'Send PIN',
    sent: 'PIN sent.',
    close: 'Close',
    pinInvalid: 'The PIN must be four digits.',
    // Pulsar seats: the operator approves requests instead of typing a PIN.
    pulsarStep1: 'Open the Pulsar app on your device.',
    pulsarStep2: 'Choose the Pulsar ID below or the seat name on the LAN.',
    pulsarStep3: 'The connection request shows here; allow or refuse it.',
    pulsarWaiting: 'Waiting for a connection request…',
    requestTitle: 'Connection request',
    requestFrom: (d: string, kind: string) => `“${d}” wants to ${kind === 'pair' ? 'pair' : 'connect'}.`,
    keyFp: 'Key fingerprint',
    passwordWrong: 'The device entered a wrong password.',
    allow: 'Allow',
    viewOnly: 'View only',
    deny: 'Refuse',
    answered: 'Answer sent.',
    otp: 'One-time password',
    showOtp: 'Show',
    refreshOtp: 'Renew',
    connectPassword: 'Permanent connection password',
    connectPasswordHint: 'Write only; the saved password is never shown.',
    connectPasswordSave: 'Save password',
    connectPasswordRemove: 'Remove password',
    connectPasswordRemoveConfirm: 'Remove the permanent connection password? Devices can no longer connect with it.',
    connectPasswordRemoveYes: 'Yes, remove',
    connectPasswordRemoveNo: 'Cancel',
    connectPasswordSaved: 'Connection password saved.',
    connectPasswordCleared: 'Connection password removed.',
    relayAuth: 'The relay server asks for a sign-in',
    relayPassword: 'Relay server password',
    relayTotp: 'Verification code (6 digits, optional)',
    relayAuthSend: 'Send the sign-in details',
    relayAuthSent: 'Sign-in details sent.',
    results: {
      allowed: 'Connection allowed.',
      allowed_view_only: 'View-only access allowed.',
      denied: 'Request refused.',
      password: 'The device connected with the password.',
      paired: 'Device paired.',
      timeout: 'The request timed out.',
      client_gone: 'The device disconnected.',
      locked_out: 'Too many wrong attempts; the device is blocked for a while.'
    } as Record<string, string>
  },
  patching: {
    install: {
      title: 'Install process patching?',
      bullets: [
        'Improves controller (HID) isolation.',
        'Makes Steam isolation usable.',
        'Fixes apps that refuse to run in a remote session.'
      ],
      question: 'Install the compatibility layer now?',
      confirm: 'Install',
      progress: 'Installing the compatibility layer…'
    },
    remove: {
      title: 'Remove process patching?',
      bullets: [
        'Controller (HID) isolation gets weaker.',
        'Steam isolation turns off.',
        'Apps that refuse to run in a remote session may stop working again.'
      ],
      question: 'Remove the compatibility layer now?',
      confirm: 'Remove',
      progress: 'Removing the compatibility layer…'
    },
    warning:
      'Compatibility layers work in Windows’ program loading path; a bug can make the whole system unusable.',
    caution: 'Be careful.',
    cancel: 'Cancel',
    rebootTitle: 'Restart needed',
    rebootBody: 'Windows must restart for the change to take full effect.',
    rebootNow: 'Restart now',
    rebootLater: 'I will restart later'
  },
  cred: {
    title: 'Testing the user credentials',
    pending: 'Trying a Windows sign-in…',
    okTitle: 'The user credentials are correct',
    okBody: 'This user can sign in with the saved password.',
    failTitle: 'Sign-in failed',
    failHint: 'Check the user name and password and save them again.',
    disabled: 'This account is disabled.',
    locked: 'This account is locked (too many failed attempts).',
    expired: 'This account’s password has expired; set a new password in Windows.',
    fix: 'Enable / unlock the account',
    confirmTitle: 'Change the account?',
    confirmBody: 'Pulsar Seat will enable this Windows account and unlock it. This changes the account itself.',
    confirm: 'Yes, change',
    retest: 'Test again',
    close: 'Close'
  },
  common: {
    examplePath: 'C:\\Games\\game.exe',
    exampleExe: 'game.exe',
    loading: 'Loading…',
    close: 'Close',
    error: 'Error'
  }
};

const remoteFieldErrorText: Record<RemoteFieldError, string> = {
  name_empty: 'Type a name.',
  name_chars: 'Only English letters and digits can be used.',
  name_long: 'The name can be at most 128 characters.',
  name_duplicate: 'A seat with this name already exists.',
  display_name: 'The display name cannot be empty or start or end with a space.',
  display_name_duplicate: 'This display name is used on another seat.',
  port: 'The port must be a number from 1029 to 65514.',
  port_overlap: 'This port range overlaps with another seat.'
};

const remoteErrorText: Record<RemoteErrorCode, string> = {
  version_mismatch: 'The manager and service versions do not match.',
  bad_request: 'Invalid request.',
  not_found: 'Seat not found.',
  invalid_name: remoteFieldErrorText.name_chars,
  duplicate_name: remoteFieldErrorText.name_duplicate,
  invalid_display_name: remoteFieldErrorText.display_name,
  invalid_port: remoteFieldErrorText.port,
  port_overlap: remoteFieldErrorText.port_overlap,
  no_free_port: 'No free port range left.',
  invalid_value: 'Invalid value.',
  invalid_user: 'The Windows user cannot be used (missing, disabled, locked or the password has expired).',
  user_in_use: 'This user is in use on another seat or on the console.',
  missing_credentials: 'Enter a user and password first.',
  seat_running: 'This cannot change while the seat is running.',
  not_implemented: 'This feature is not ready yet (it comes in a later stage).',
  store: 'The settings could not be saved.',
  service_unavailable: 'The service or the seat’s stream server cannot be reached.',
  internal: 'An unexpected error occurred.',
  unsupported: 'This feature is not available on this PC.',
  edition_unsupported: 'This Windows edition does not support multiseat.',
  coexistence: 'Nothing was done because another multiseat product was found.',
  seat_failed: 'The seat could not start.',
  timeout: 'The operation timed out.',
  driver_missing: 'A needed driver is not installed.',
  policy_blocked: 'A group policy blocks this action.',
  warning_not_acknowledged: 'Read and acknowledge the warning first.',
  no_pending_request: 'There is no pending pairing request.',
  wrong_pin: 'Wrong PIN.',
  hash_mismatch: 'The file could not be verified.',
  firewall: 'The firewall could not be set up.'
};

export const en = {
  t,
  productText,
  safeModeText,
  failureText,
  errorText,
  issueText,
  noticeText,
  loopbackStateText,
  misc,
  rt,
  remoteFieldErrorText,
  remoteErrorText
};

/** Shape of one locale (English is the reference). */
export type Locale = typeof en;
