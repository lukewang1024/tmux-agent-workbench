ObjC.import('Cocoa');

function run(argv) {
  var title = argv[0] || 'Workbench';
  var body = argv[1] || '';
  var executable = argv[2] || '';
  var session = argv[3] || '';
  var windowId = argv[4] || '';
  var pane = argv[5] || '';
  var remoteId = argv[6] || '';
  var tmuxSocket = argv[7] || '';
  $.NSApplication.sharedApplication;
  $.NSApp.setActivationPolicy($.NSApplicationActivationPolicyAccessory);
  var screen = $.NSScreen.mainScreen.visibleFrame;
  var width = 460, height = 76;
  var rect = $.NSMakeRect(screen.origin.x + (screen.size.width - width) / 2,
                          screen.origin.y + screen.size.height - height - 24,
                          width, height);
  var window = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer(
    rect, $.NSWindowStyleMaskBorderless, $.NSBackingStoreBuffered, false);
  window.level = $.NSFloatingWindowLevel;
  window.opaque = false;
  window.backgroundColor = $.NSColor.colorWithWhiteAlpha(0.12, 0.94);
  window.hasShadow = true;
  var field = $.NSTextField.alloc.initWithFrame($.NSMakeRect(20, 14, width - 40, 48));
  field.editable = false; field.bezeled = false; field.drawsBackground = false;
  field.textColor = $.NSColor.whiteColor;
  field.font = $.NSFont.systemFontOfSizeWeight(15, $.NSFontWeightMedium);
  field.stringValue = title + '\n' + body;
  window.contentView.addSubview(field);
  if (executable && /^\$[0-9]+$/.test(session) && /^@[0-9]+$/.test(windowId) && /^%[0-9]+$/.test(pane)) {
    ObjC.registerSubclass({
      name: 'WorkbenchOverlayClickHandler',
      superclass: 'NSObject',
      methods: {
        'handleClick:': {
          types: ['void', ['id']],
          implementation: function() {
            var task = $.NSTask.alloc.init;
            task.setLaunchPath($(executable));
            if (remoteId && tmuxSocket) {
              task.setArguments($(['relay', 'focus-click', '--remote-id', remoteId,
                '--tmux-socket', tmuxSocket, '--session-id', session, '--pane-id', pane]));
            } else {
              task.setArguments($(['focus', '--session', session, '--window', windowId, '--pane', pane]));
            }
            task.launch;
            $.NSApp.terminate(null);
          }
        }
      }
    });
    var handler = $.WorkbenchOverlayClickHandler.alloc.init;
    var button = $.NSButton.alloc.initWithFrame($.NSMakeRect(0, 0, width, height));
    button.transparent = true;
    button.bordered = false;
    button.target = handler;
    button.action = 'handleClick:';
    window.contentView.addSubview(button);
    ObjC.bindFunction('objc_setAssociatedObject', ['void', ['id', 'id', 'id', 'long']]);
    $.objc_setAssociatedObject(window, 'workbench-handler', handler, 1);
  }
  window.orderFrontRegardless;
  $.NSTimer.scheduledTimerWithTimeIntervalTargetSelectorUserInfoRepeats(4, $.NSApp, 'terminate:', null, false);
  $.NSApp.run;
}
