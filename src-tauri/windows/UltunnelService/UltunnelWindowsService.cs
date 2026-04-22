using System.Diagnostics;
using System.ServiceProcess;
using System.Text.Json;
using System.Timers;
using UltunnelService.Models;

namespace UltunnelService;

public class UltunnelWindowsService : ServiceBase {
	private readonly string _baseDir;
	private readonly string _commandFile;
	private readonly string _statusFile;
	private readonly string _logFile;
	private readonly string _singBoxPath;

	private readonly string _configPath;

	// private readonly string _applicationDataPath;
	private string _lastProcessedCommandId;

	private System.Timers.Timer? _timer;
	private Process? _singBoxProcess;
	private long _lastTimestamp;

	public UltunnelWindowsService() {
		ServiceName = "UltunnelService";
		CanStop = true;
		AutoLog = true;

		// _applicationDataPath = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
		_baseDir = Path.Combine(
			Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
			"Ultunnel"
		);

		_commandFile = Path.Combine(_baseDir, "command.json");
		_statusFile = Path.Combine(_baseDir, "status.json");
		_logFile = Path.Combine(_baseDir, "service.log");
		_singBoxPath = Path.Combine(_baseDir, "sing-box.exe");
		_configPath = Path.Combine(_baseDir, "singbox.json");
	}

	protected override void OnStart(string[] args) {
		Directory.CreateDirectory(_baseDir);
		Log("Service started");

		_currentStatus = new ServiceStatus {
			Running = IsSingBoxRunning(),
			Message = "Service started",
			Pid = _singBoxProcess?.Id
		};
		WriteStatus(_currentStatus);

		_timer = new System.Timers.Timer(1000);
		_timer.Elapsed += OnTick;
		_timer.AutoReset = true;
		_timer.Start();
	}

	protected override void OnStop() {
		_timer?.Stop();
		_timer?.Dispose();
		_timer = null;

		StopSingBox();
		WriteStatus(new ServiceStatus {
			Running = false,
			Message = "Service stopped",
			Pid = null
		});
		Log("Service stopped");
	}

	private void OnTick(object? sender, ElapsedEventArgs e) {
		try {
			HandleCommand();

			_currentStatus.Running = IsSingBoxRunning();
			_currentStatus.Pid = _singBoxProcess?.Id;
			if (string.IsNullOrWhiteSpace(_currentStatus.Message))
				_currentStatus.Message = "OK";

			WriteStatus(_currentStatus);
		}
		catch (Exception ex) {
			Log("Tick error: " + ex);

			_currentStatus.Running = IsSingBoxRunning();
			_currentStatus.Pid = _singBoxProcess?.Id;
			_currentStatus.Message = ex.Message;

			WriteStatus(_currentStatus);
		}
	}

	private void HandleCommand() {
		if (!File.Exists(_commandFile)) {
			Log("Command file not found: " + _commandFile);
			return;
		}
		var json = File.ReadAllText(_commandFile);
		if (string.IsNullOrWhiteSpace(json)) {
			Log("string.IsNullOrWhiteSpace(json): ");
			return;
		}
		var cmd = JsonSerializer.Deserialize<ServiceCommand>(json);
		if (cmd == null || string.IsNullOrWhiteSpace(cmd.Id) || string.IsNullOrWhiteSpace(cmd.Cmd)) {
			Log("cmd == null || string.IsNullOrWhiteSpace(cmd.Id) || string.IsNullOrWhiteSpace(cmd.Cmd)");
			return;
		}
		if (cmd.Id == _lastProcessedCommandId) {
			return;
		}
		_lastProcessedCommandId = cmd.Id;

		switch (cmd.Cmd.Trim().ToLowerInvariant()) {
			case "start":
				StartSingBox(cmd.ConfigPath);
				WriteStatus(new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = IsSingBoxRunning(),
					Pid = _singBoxProcess?.Id,
					Message = "started"
				});
				break;

			case "stop":
				StopSingBox();
				WriteStatus(new ServiceStatus {
					LastCommandId = cmd.Id,
					Running = false,
					Message = "stopped"
				});
				break;
		}
	}

	private void StartSingBox(string? cmdConfigPath) {
		if (IsSingBoxRunning())
			return;

		if (!File.Exists(_singBoxPath)) {
			throw new FileNotFoundException("sing-box.exe not found", _singBoxPath);
		}

		if (!File.Exists(_configPath)) {
			throw new FileNotFoundException("singbox.json not found", _configPath);
		}

		var psi = new ProcessStartInfo {
			FileName = _singBoxPath,
			Arguments = $"run -c \"{_configPath}\"",
			WorkingDirectory = _baseDir,
			UseShellExecute = false,
			CreateNoWindow = true
		};

		_singBoxProcess = Process.Start(psi);
		Log("sing-box started, pid=" + _singBoxProcess?.Id);
	}

	private void StopSingBox() {
		try {
			if (_singBoxProcess != null && !_singBoxProcess.HasExited) {
				_singBoxProcess.Kill(true);
				_singBoxProcess.WaitForExit(5000);
			}
		}
		catch (Exception ex) {
			Log("Stop tracked process error: " + ex.Message);
		}

		_singBoxProcess = null;

		foreach (var p in Process.GetProcessesByName("sing-box")) {
			try {
				p.Kill(true);
			}
			catch
			{
			}
		}

		Log("sing-box stopped");
	}


	private bool IsSingBoxRunning() {
		if (_singBoxProcess != null && !_singBoxProcess.HasExited)
			return true;

		return Process.GetProcessesByName("sing-box").Length > 0;
	}


	private void WriteStatus(ServiceStatus status) {
		Directory.CreateDirectory(_baseDir);

		string json = JsonSerializer.Serialize(status, new JsonSerializerOptions {
			WriteIndented = true
		});

		File.WriteAllText(_statusFile, json);
	}


	private void Log(string message) {
		File.AppendAllText(
			_logFile,
			$"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] {message}{Environment.NewLine}"
		);
	}

	private ServiceStatus _currentStatus = new ServiceStatus {
		Running = false,
		Message = "initialized",
		Pid = null
	};
}