namespace SubtitleAdder.Core;

public sealed record TranslationModelChoice(string Id, string Title);

/// <summary>Provider-scoped choices and selection that keep cloud, LAN, and on-device model settings separate.</summary>
public static class TranslationModelSelection
{
    public static IReadOnlyList<TranslationModelChoice> GetModels(
        TranslationProviderManager manager, SubtitleViewModel viewModel, Guid? providerId)
    {
        if (providerId is null)
        {
            return viewModel.Catalog.TranslationModels
                .Select(model => new TranslationModelChoice(model.Id, ModelLabel.MenuTitle(model))).ToList();
        }

        var provider = manager.Providers.FirstOrDefault(p => p.Id == providerId);
        if (provider is null) return [];

        // A discovered list belongs to this provider. Never mix in LAN settings or another provider's models.
        var models = provider.DiscoveredModels.Count > 0 ? provider.DiscoveredModels : provider.AvailableModels;
        return models.Append(provider.Model)
            .Where(model => !string.IsNullOrWhiteSpace(model))
            .Select(model => model.Trim())
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .OrderBy(model => model, StringComparer.OrdinalIgnoreCase)
            .Select(model => new TranslationModelChoice(model, model)).ToList();
    }

    public static bool SelectProvider(TranslationProviderManager manager, SubtitleViewModel viewModel, Guid? providerId)
    {
        // A stale picker must not interpret a deleted provider as a request to switch to on-device translation.
        if (providerId is not null && !manager.Providers.Any(p => p.Id == providerId)) return false;
        manager.SetActiveProvider(providerId);
        manager.ApplyToViewModel(viewModel);
        return true;
    }

    public static bool SelectModel(
        TranslationProviderManager manager, SubtitleViewModel viewModel, Guid? providerId, string modelId)
    {
        if (!GetModels(manager, viewModel, providerId).Any(m => m.Id == modelId)) return false;
        if (providerId is { } id)
        {
            manager.UpdateProviderModel(id, modelId);
            return SelectProvider(manager, viewModel, id);
        }

        SelectProvider(manager, viewModel, null);
        viewModel.TranslationModel = modelId;
        return true;
    }
}
