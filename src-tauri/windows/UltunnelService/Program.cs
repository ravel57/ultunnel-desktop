using System.Diagnostics;
using System.Security.Principal;
using System.ServiceProcess;
using System.Text.Json;
using UltunnelService;

internal static class Program {
	private const string ServiceName = "UltunnelService";

	static int Main(string[] args) {
		string mode = args.FirstOrDefault()?.ToLowerInvariant() ?? "";

		try {
			switch (mode) {
				case "--service":
					ServiceBase.Run(new UltunnelWindowsService());
					return 0;

				case "--install":
					RequireAdministrator();
					InstallService();
					return 0;

				case "--uninstall":
					RequireAdministrator();
					UninstallService();
					return 0;

				case "--start":
					SendCommand("start");
					return 0;

				case "--stop":
					SendCommand("stop");
					return 0;

				case "--restart":
					SendCommand("restart");
					return 0;

				case "--status":
					PrintStatus();
					return 0;

				default:
					// return RunClientMode();
					RequireAdministrator();
					InstallService();
					return 0;
			}
		}
		catch (Exception ex) {
			Console.Error.WriteLine(ex);
			return 1;
		}
	}

	private static int RunClientMode() {
		if (!ServiceExists(ServiceName)) {
			RelaunchSelfAsAdmin("--install");
			return 0;
		}

		// Здесь можно запускать GUI
		// Например WinForms / WPF / Avalonia / Tauri-host / консольное меню
		Console.WriteLine("Client mode");
		Console.WriteLine("Service installed");
		Console.WriteLine("1 - start");
		Console.WriteLine("2 - stop");
		Console.WriteLine("3 - status");

		string? key = Console.ReadLine();
		switch (key) {
			case "1":
				SendCommand("start");
				break;
			case "2":
				SendCommand("stop");
				break;
			case "3":
				PrintStatus();
				break;
		}

		return 0;
	}

	private static void InstallService() {
		string installDir = Path.Combine(
			Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
			"Ultunnel"
		);

		Directory.CreateDirectory(installDir);
		
		string currentDir = AppContext.BaseDirectory;

		string targetExe = Path.Combine(installDir, "UltunnelService.exe");
		CopyDirectory(currentDir, installDir);

		if (ServiceExists(ServiceName)) {
			StopServiceIfRunning();
			DeleteService();
		}

		RunSc($"create {ServiceName} binPath= \"\\\"{targetExe}\\\" --service\" start= auto");
		RunSc($"description {ServiceName} \"Ultunnel background service\"");
		RunSc($"start {ServiceName}", ignoreExitCode: true);

		Console.WriteLine("Service installed");
	}

	private static void UninstallService() {
		if (!ServiceExists(ServiceName))
			return;

		StopServiceIfRunning();
		DeleteService();
		Console.WriteLine("Service removed");
	}

	private static void SendCommand(string cmd) {
		string baseDir = Path.Combine(
			Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
			"Ultunnel"
		);

		Directory.CreateDirectory(baseDir);

		string commandFile = Path.Combine(baseDir, "command.json");

		var payload = new {
			cmd = cmd,
			timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()
		};

		string json = JsonSerializer.Serialize(payload, new JsonSerializerOptions {
			WriteIndented = true
		});

		File.WriteAllText(commandFile, json);
		Console.WriteLine($"Command sent: {cmd}");
	}

	private static void PrintStatus() {
		string baseDir = Path.Combine(
			Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
			"Ultunnel"
		);

		string statusFile = Path.Combine(baseDir, "status.json");

		if (!File.Exists(statusFile)) {
			Console.WriteLine("status.json not found");
			return;
		}

		Console.WriteLine(File.ReadAllText(statusFile));
	}

	private static void RequireAdministrator() {
		if (IsAdministrator()) {
			return;
		}

		throw new InvalidOperationException("Administrator rights required");
	}

	private static bool IsAdministrator() {
		using var identity = WindowsIdentity.GetCurrent();
		var principal = new WindowsPrincipal(identity);
		return principal.IsInRole(WindowsBuiltInRole.Administrator);
	}

	private static void RelaunchSelfAsAdmin(string arguments) {
		var psi = new ProcessStartInfo {
			FileName = Environment.ProcessPath!,
			Arguments = arguments,
			UseShellExecute = true,
			Verb = "runas"
		};

		Process.Start(psi);
	}

	private static bool ServiceExists(string serviceName) {
		var psi = new ProcessStartInfo {
			FileName = "sc.exe",
			Arguments = $"query {serviceName}",
			RedirectStandardOutput = true,
			RedirectStandardError = true,
			UseShellExecute = false,
			CreateNoWindow = true
		};

		using var process = Process.Start(psi)!;
		process.WaitForExit();
		return process.ExitCode == 0;
	}

	private static void StopServiceIfRunning() {
		RunSc($"stop {ServiceName}", ignoreExitCode: true);
		Thread.Sleep(2000);
	}

	private static void DeleteService() {
		RunSc($"delete {ServiceName}", ignoreExitCode: true);
		Thread.Sleep(1500);
	}

	private static void RunSc(string arguments, bool ignoreExitCode = false) {
		var psi = new ProcessStartInfo {
			FileName = "sc.exe",
			Arguments = arguments,
			RedirectStandardOutput = true,
			RedirectStandardError = true,
			UseShellExecute = false,
			CreateNoWindow = true
		};

		using var process = Process.Start(psi)!;
		string stdout = process.StandardOutput.ReadToEnd();
		string stderr = process.StandardError.ReadToEnd();
		process.WaitForExit();

		if (!ignoreExitCode && process.ExitCode != 0) {
			throw new InvalidOperationException(
				$"sc.exe {arguments} failed\nOUT:\n{stdout}\nERR:\n{stderr}"
			);
		}
	}

	static void CopyDirectory(string sourceDir, string destinationDir, bool recursive = true) {
		var dir = new DirectoryInfo(sourceDir);
		if (!dir.Exists) {
			throw new DirectoryNotFoundException($"Исходная папка не найдена: {sourceDir}");
		}
		Directory.CreateDirectory(destinationDir);
		foreach (FileInfo file in dir.GetFiles()) {
			string targetFilePath = Path.Combine(destinationDir, file.Name);
			file.CopyTo(targetFilePath, true);
		}
		if (!recursive) {
			return;
		}
		foreach (DirectoryInfo subDir in dir.GetDirectories()) {
			string newDestinationDir = Path.Combine(destinationDir, subDir.Name);
			CopyDirectory(subDir.FullName, newDestinationDir);
		}
	}
}