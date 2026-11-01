namespace SubtitleAdder.Core;

/// <summary>
/// A small UndoManager: undo and redo stacks of named groups. Registering while undoing fills the redo stack,
/// while redoing the undo stack; any other registration clears the redo stack. Registrations inside
/// <see cref="BeginGroup"/> … dispose form one group; outside a group each registration is its own group.
/// Action names are catalog keys, localized when shown.
/// </summary>
public sealed class UndoStack
{
    private sealed class Group(string actionKey)
    {
        public string ActionKey { get; set; } = actionKey;
        public List<Action> Actions { get; } = [];
    }

    private readonly List<Group> _undo = [];
    private readonly List<Group> _redo = [];
    private Group? _open;
    private int _depth;

    /// <summary>Raised whenever the stacks change (CanUndo/CanRedo/names may differ).</summary>
    public event EventHandler? Changed;

    public bool IsUndoing { get; private set; }
    public bool IsRedoing { get; private set; }
    public bool CanUndo => _undo.Count > 0;
    public bool CanRedo => _redo.Count > 0;

    /// <summary>Catalog key of the action <see cref="Undo"/> would revert, or <c>null</c>.</summary>
    public string? UndoActionKey => _undo.Count > 0 ? _undo[^1].ActionKey : null;

    /// <summary>Catalog key of the action <see cref="Redo"/> would repeat, or <c>null</c>.</summary>
    public string? RedoActionKey => _redo.Count > 0 ? _redo[^1].ActionKey : null;

    /// <summary>Opens a group (nestable); everything registered until the returned handle is disposed undoes together.</summary>
    public IDisposable BeginGroup()
    {
        _depth++;
        return new GroupScope(this);
    }

    /// <summary>Records how to revert the change just made.</summary>
    public void Register(string actionKey, Action undo)
    {
        if (_open is { } open)
        {
            open.Actions.Add(undo);
            open.ActionKey = actionKey;
            return;
        }
        var group = new Group(actionKey);
        group.Actions.Add(undo);
        if (_depth > 0 && !IsUndoing && !IsRedoing)
        {
            _open = group;
            return;
        }
        Push(group);
    }

    public void Undo() => Run(_undo, undoing: true);

    public void Redo() => Run(_redo, undoing: false);

    public void Clear()
    {
        _undo.Clear();
        _redo.Clear();
        _open = null;
        Changed?.Invoke(this, EventArgs.Empty);
    }

    private void Run(List<Group> stack, bool undoing)
    {
        if (stack.Count == 0 || IsUndoing || IsRedoing) return;
        var group = stack[^1];
        stack.RemoveAt(stack.Count - 1);
        // Registrations made while reverting are collected into the opposite group.
        var inverse = new Group(group.ActionKey);
        var savedOpen = _open;
        _open = inverse;
        if (undoing) IsUndoing = true;
        else IsRedoing = true;
        try
        {
            for (var i = group.Actions.Count - 1; i >= 0; i--) group.Actions[i]();
        }
        finally
        {
            IsUndoing = false;
            IsRedoing = false;
            _open = savedOpen;
        }
        inverse.ActionKey = group.ActionKey;
        if (inverse.Actions.Count > 0) (undoing ? _redo : _undo).Add(inverse);
        Changed?.Invoke(this, EventArgs.Empty);
    }

    private void Push(Group group)
    {
        if (IsUndoing) _redo.Add(group);
        else if (IsRedoing) _undo.Add(group);
        else
        {
            _undo.Add(group);
            _redo.Clear();
        }
        Changed?.Invoke(this, EventArgs.Empty);
    }

    private void EndGroup()
    {
        if (_depth == 0) return;
        _depth--;
        if (_depth > 0 || IsUndoing || IsRedoing || _open is not { } group) return;
        _open = null;
        Push(group);
    }

    private sealed class GroupScope(UndoStack owner) : IDisposable
    {
        private bool _disposed;

        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;
            owner.EndGroup();
        }
    }
}
