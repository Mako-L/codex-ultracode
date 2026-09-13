export function supportedApprovalPolicy(policy) {
  return ['never', 'on-request', 'untrusted'].includes(policy);
}

export function approvalNeedsRelay(policy) {
  return supportedApprovalPolicy(policy) && policy !== 'never';
}

const sandboxes = ['read-only', 'workspace-write', 'danger-full-access'];

export function sandboxWithinCeiling(requested, ceiling) {
  const requestRank = sandboxes.indexOf(requested);
  const ceilingRank = sandboxes.indexOf(ceiling);
  return requestRank >= 0 && ceilingRank >= 0 && requestRank <= ceilingRank;
}
