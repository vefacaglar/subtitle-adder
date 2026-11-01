using System.Diagnostics;
using System.Text;
using System.Text.RegularExpressions;

// Scriptable stand-in for subtitle-engine. Usage: FakeEngine <script file> <engine arguments...>
// The script is run line by line; ${NAME} in text is replaced by the environment variable NAME.
//   out <text>       write a line to stdout          err <text>    write a line to stderr
//   args <file>      write the engine arguments (space-joined) to a file
//   stdin <file>     copy stdin to a file            cat           copy stdin to stdout
//   drain            read and discard stdin          sleep <ms>    sleep
//   pid <file>       write this process id           child <file>  start a sleeping child, write its id
//   exit <code>      exit immediately
// "FakeEngine --sleep-child" just sleeps (the grandchild used to test process-tree kills).

var utf8 = new UTF8Encoding(false);
if (args.Length > 0 && args[0] == "--sleep-child")
{
    Thread.Sleep(30_000);
    return 0;
}

var stdout = new StreamWriter(Console.OpenStandardOutput(), utf8) { AutoFlush = true, NewLine = "\n" };
var stderr = new StreamWriter(Console.OpenStandardError(), utf8) { AutoFlush = true, NewLine = "\n" };
var engineArgs = args.Skip(1).ToArray();

foreach (var raw in File.ReadAllLines(args[0], utf8))
{
    var line = Regex.Replace(raw, @"\$\{(\w+)\}", m => Environment.GetEnvironmentVariable(m.Groups[1].Value) ?? "");
    var space = line.IndexOf(' ');
    var command = space < 0 ? line : line[..space];
    var rest = space < 0 ? "" : line[(space + 1)..];
    switch (command)
    {
        case "out":
            stdout.WriteLine(rest);
            break;
        case "err":
            stderr.WriteLine(rest);
            break;
        case "args":
            File.WriteAllText(rest, string.Join(" ", engineArgs), utf8);
            break;
        case "stdin":
            using (var file = File.Create(rest)) Console.OpenStandardInput().CopyTo(file);
            break;
        case "cat":
            Console.OpenStandardInput().CopyTo(Console.OpenStandardOutput());
            break;
        case "drain":
            Console.OpenStandardInput().CopyTo(Stream.Null);
            break;
        case "sleep":
            Thread.Sleep(int.Parse(rest));
            break;
        case "pid":
            File.WriteAllText(rest, Environment.ProcessId.ToString());
            break;
        case "child":
            var child = Process.Start(new ProcessStartInfo(Environment.ProcessPath!, "--sleep-child") { UseShellExecute = false })!;
            File.WriteAllText(rest, child.Id.ToString());
            break;
        case "exit":
            return int.Parse(rest);
        case "":
            break;
        default:
            stderr.WriteLine($"unknown fake engine command: {command}");
            return 99;
    }
}
return 0;
