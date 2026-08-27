export function createOperationLog(limit = 1024) {
  const completed = new Map();
  return {
    claim(operationId) {
      if (!operationId) return { ok: false, error: "operation_id is required" };
      if (completed.has(operationId)) {
        return { ok: true, duplicate: true };
      }
      completed.set(operationId, Date.now());
      while (completed.size > limit) completed.delete(completed.keys().next().value);
      return { ok: true, duplicate: false };
    },
  };
}
