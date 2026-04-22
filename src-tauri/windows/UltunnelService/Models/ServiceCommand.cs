using System.Text.Json.Serialization;

namespace UltunnelService.Models;

public class ServiceCommand {
	[JsonPropertyName("id")]
	public string? Id { get; set; }

	[JsonPropertyName("cmd")]
	public string? Cmd { get; set; }

	[JsonPropertyName("configPath")]
	public string? ConfigPath { get; set; }

	[JsonPropertyName("timestamp")]
	public long Timestamp { get; set; }
}