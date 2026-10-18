export const integrityZh = {
  "integrity.open": "校验依赖完整性",
  "integrity.starting": "正在准备校验…",
  "integrity.help":
    "读取依赖并核对声明的 SHA1 和大小。发现损坏会保留原文件，不自动替换。",
  "integrity.failed": "无法开始完整性检查",
  "integrity.step": "校验依赖完整性",
  "integrity.result": "依赖完整性检查结果",
  "integrity.dismiss": "收起检查结果",
  "integrity.verified": "已验证",
  "integrity.missing": "缺失",
  "integrity.corrupt": "损坏",
  "integrity.unverifiable": "无法校验",
  "integrity.complete": "检查完成，未修改文件。",
  "integrity.coverage": "资源索引未通过检查，资源对象尚未完整校验。",
  "integrity.problems": "查看 {count} 项检查发现",
  "integrity.sizeMismatch": "大小不符",
  "integrity.sha1Mismatch": "SHA1 不符",
  "integrity.omitted": "另有 {count} 项未显示。",
  "integrity.resultUnavailable": "完整性检查结果不可用，请重新检查。",
} as const;
export const integrityEn = {
  "integrity.open": "Verify dependency integrity",
  "integrity.starting": "Preparing verification…",
  "integrity.help":
    "Read dependencies and compare their declared SHA1 and size. Damaged files are preserved and require manual repair.",
  "integrity.failed": "Integrity check could not start",
  "integrity.step": "Verify dependency integrity",
  "integrity.result": "Dependency integrity results",
  "integrity.dismiss": "Dismiss verification results",
  "integrity.verified": "Verified",
  "integrity.missing": "Missing",
  "integrity.corrupt": "Damaged",
  "integrity.unverifiable": "Unverifiable",
  "integrity.complete": "Check complete. Files were not modified.",
  "integrity.coverage":
    "The asset index could not be verified. Asset object coverage is incomplete.",
  "integrity.problems": "View {count} findings",
  "integrity.sizeMismatch": "Size mismatch",
  "integrity.sha1Mismatch": "SHA1 mismatch",
  "integrity.omitted": "{count} more findings are not shown.",
  "integrity.resultUnavailable":
    "Integrity results are unavailable. Run the check again.",
} as const satisfies Record<keyof typeof integrityZh, string>;
