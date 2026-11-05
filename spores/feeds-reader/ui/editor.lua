-- Spike placeholder. The first UI can render the same records collection.
return function(state)
  return {
    title = state.config and state.config.title or "Feeds",
    rows = state.items or {},
  }
end
