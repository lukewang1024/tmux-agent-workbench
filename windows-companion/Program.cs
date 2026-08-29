using System.Diagnostics;
using System.Text.RegularExpressions;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using System.Windows.Forms;

internal static class Program
{
    private static readonly Regex OpaqueId = new("^[A-Za-z0-9._:-]{1,256}$", RegexOptions.CultureInvariant);

    [STAThread]
    private static int Main(string[] args)
    {
        if (args.Length == 0) return Usage();
        return args[0] switch
        {
            "setup" => Setup(),
            "notify" => Notify(args),
            "clipboard-read" => ClipboardRead(),
            "clipboard-write" => ClipboardWrite(),
            "activate" => Activate(args),
            _ => Usage(),
        };
    }

    private static int Setup()
    {
        var destination = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "tmux-agent-workbench");
        Directory.CreateDirectory(destination);
        var installed = Path.Combine(destination, "wb-client.exe");
        var current = Environment.ProcessPath ?? throw new InvalidOperationException("process path unavailable");
        if (!Path.GetFullPath(current).Equals(Path.GetFullPath(installed), StringComparison.OrdinalIgnoreCase))
        {
            File.Copy(current, installed, true);
            return Process.Start(new ProcessStartInfo(installed, "setup") { UseShellExecute = false })!.WaitForExitCode();
        }
        AppNotificationManager.Default.Register();
        AppNotificationManager.Default.Unregister();
        Console.WriteLine($"notification activation registered; install directory: {destination}");
        return 0;
    }

    private static int Notify(string[] args)
    {
        if (args.Length != 4 || !OpaqueId.IsMatch(args[1])) return Usage();
        AppNotificationManager.Default.NotificationInvoked += (_, activation) => ActivateArgument(activation.Argument);
        AppNotificationManager.Default.Register();
        var notification = new AppNotificationBuilder()
            .AddArgument("event", args[1])
            .AddText(args[2])
            .AddText(args[3])
            .BuildNotification();
        AppNotificationManager.Default.Show(notification);
        AppNotificationManager.Default.Unregister();
        return 0;
    }

    private static int Activate(string[] args)
    {
        if (args.Length != 2) return Usage();
        return ActivateArgument(args[1]);
    }

    private static int ActivateArgument(string argument)
    {
        var eventId = argument.Split('&', StringSplitOptions.RemoveEmptyEntries)
            .Select(part => part.Split('=', 2))
            .FirstOrDefault(part => part.Length == 2 && part[0] == "event")?[1];
        if (eventId is null || !OpaqueId.IsMatch(eventId)) return 2;
        var wt = FindOnPath("wt.exe");
        var executable = wt ?? "wsl.exe";
        var arguments = wt is null
            ? new[] { "wb", "attach", "--target", eventId }
            : new[] { "-w", "0", "new-tab", "wsl.exe", "wb", "attach", "--target", eventId };
        var startInfo = new ProcessStartInfo(executable) { UseShellExecute = false };
        startInfo.WithArguments(arguments);
        Process.Start(startInfo);
        return 0;
    }

    private static int ClipboardRead()
    {
        var text = Clipboard.ContainsText(TextDataFormat.UnicodeText) ? Clipboard.GetText(TextDataFormat.UnicodeText) : "";
        if (text.Contains('\0') || System.Text.Encoding.UTF8.GetByteCount(text) > 1024 * 1024) return 3;
        Console.OutputEncoding = System.Text.Encoding.UTF8;
        Console.Write(text);
        return 0;
    }

    private static int ClipboardWrite()
    {
        Console.InputEncoding = System.Text.Encoding.UTF8;
        var text = Console.In.ReadToEnd();
        if (text.Contains('\0') || System.Text.Encoding.UTF8.GetByteCount(text) > 1024 * 1024) return 3;
        Clipboard.SetText(text, TextDataFormat.UnicodeText);
        return 0;
    }

    private static string? FindOnPath(string name) =>
        (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator)
            .Select(directory => Path.Combine(directory, name)).FirstOrDefault(File.Exists);

    private static int Usage()
    {
        Console.Error.WriteLine("usage: wb-client setup|notify <event-id> <title> <body>|clipboard-read|clipboard-write|activate <argument>");
        return 2;
    }
}

internal static class ProcessStartInfoExtensions
{
    internal static ProcessStartInfo WithArguments(this ProcessStartInfo info, IEnumerable<string> arguments)
    {
        foreach (var argument in arguments) info.ArgumentList.Add(argument);
        return info;
    }
}

internal static class ProcessExtensions
{
    internal static int WaitForExitCode(this Process process) { process.WaitForExit(); return process.ExitCode; }
}
