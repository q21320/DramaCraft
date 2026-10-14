/**
 * 导航单一来源：菜单项、页头标题与该页职责说明都在这里，避免各页文案对不上。
 */
export interface NavItem {
  key: string;
  path: string;
  label: string;
  /** 页头副标题：这一页到底负责什么 */
  subtitle: string;
}

export const navItems: NavItem[] = [
  {
    key: "home",
    path: "/",
    label: "项目",
    subtitle: "一个项目 = 一部短剧素材 + 它的切片与分发记录",
  },
  {
    key: "library",
    path: "/library",
    label: "素材库",
    subtitle: "导入长剧素材，自动探测时长、分辨率与音轨",
  },
  {
    key: "slice",
    path: "/slice",
    label: "切片工作台",
    subtitle: "按秒数或份数切分，先预览计划再执行，产出可发布片段",
  },
  {
    key: "subtitle",
    path: "/subtitle",
    label: "字幕",
    subtitle: "ASR 生成字幕轴、导入 SRT、逐条校对",
  },
  {
    key: "export",
    path: "/export",
    label: "导出",
    subtitle: "烧字幕、转竖屏、导出成片",
  },
  {
    key: "draft",
    path: "/draft",
    label: "剪映草稿",
    subtitle: "把切片与字幕拼成剪映草稿，继续手工精修",
  },
  {
    key: "transform",
    path: "/transform",
    label: "画面处理",
    subtitle: "镜像/裁切/调色/变速，让每个片段彼此不同",
  },
  {
    key: "publish",
    path: "/publish",
    label: "发布",
    subtitle: "整理发布包（视频/封面/字幕/标题标签）并记录发布台账",
  },
  {
    key: "series",
    path: "/series",
    label: "剧目与集",
    subtitle: "按文件名重组剧目；用片头指纹检测集边界并按集重切",
  },
  {
    key: "queue",
    path: "/queue",
    label: "任务队列",
    subtitle: "把长任务排成一队顺序跑，关掉应用再打开会接着跑",
  },
  {
    key: "proof",
    path: "/proof",
    label: "证明材料",
    subtitle: "模板图填数据，一键出可直接提交的证明图",
  },
  {
    key: "settings",
    path: "/settings",
    label: "设置",
    subtitle: "指定 ffmpeg / ffprobe / Python 路径，查看各组件可用性",
  },
];

export function navByPath(path: string): NavItem | undefined {
  return navItems.find((n) => n.path === path);
}
