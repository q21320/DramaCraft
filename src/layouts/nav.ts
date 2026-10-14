/**
 * 导航单一来源：菜单项、页头标题与该页职责说明都在这里，避免各页文案对不上。
 *
 * ## 为什么分组
 *
 * 一开始这里是 14 个平铺的功能页。用户的原话是「看着好乱，能不能只保留
 * 流水线的内容，这样我都看不懂」—— 是对的。
 *
 * 一旦有了流水线，日常 90% 的操作就是「指一个源目录、点开始」。
 * 其余那些单步工具是**修某一件小事**时才用得上的（比如这一集的字幕要单独
 * 校一遍）。把它们和流水线平铺在一起，等于让用户每次都从 14 个选项里挑。
 *
 * 所以：**流水线单独摆在最上面，其余按用途收进默认折叠的分组**。
 * 第一眼看到的是一个动作，不是一个菜单。
 */
export interface NavItem {
  key: string;
  path: string;
  label: string;
  /** 页头副标题：这一页到底负责什么 */
  subtitle: string;
}

export interface NavGroup {
  key: string;
  title: string;
  /** 一句话说明这组什么时候用得上 */
  hint: string;
  items: NavItem[];
}

/** 主入口。直接摆在菜单最上面，不藏在任何分组里。 */
export const primaryNav: NavItem = {
  key: "pipeline",
  path: "/pipeline",
  label: "流水线",
  subtitle: "指一个源目录，从头到尾把一批剧做完（一部做完再做下一部）",
};

/**
 * 其余按用途分组，**默认全部收起**。
 *
 * 顺序按「从流水线出来之后最可能想干什么」排，不按实现顺序排。
 */
export const navGroups: NavGroup[] = [
  {
    key: "tools",
    title: "单步工具",
    hint: "只想处理某一件小事时用（流水线里已经自动做了这些）",
    items: [
      {
        key: "slice",
        path: "/slice",
        label: "切片",
        subtitle: "按秒数或份数切分，先预览计划再执行",
      },
      {
        key: "subtitle",
        path: "/subtitle",
        label: "字幕",
        subtitle: "识别字幕轴、导入 SRT、逐条校对",
      },
      {
        key: "transform",
        path: "/transform",
        label: "画面处理",
        subtitle: "镜像 / 裁切 / 调色 / 变速，让每个片段彼此不同",
      },
      {
        key: "export",
        path: "/export",
        label: "导出成片",
        subtitle: "烧字幕、转竖屏、导出可直接上传的成片",
      },
      {
        key: "draft",
        path: "/draft",
        label: "剪映草稿",
        subtitle: "把切片与字幕拼成剪映草稿，继续手工精修",
      },
      {
        key: "publish",
        path: "/publish",
        label: "发布包与台账",
        subtitle: "整理发布包（视频 / 封面 / 字幕 / 标题标签）并记录发布台账",
      },
      {
        key: "series",
        path: "/series",
        label: "剧目与集",
        subtitle: "按文件名重组剧目；用片头指纹检测集边界并按集重切",
      },
    ],
  },
  {
    key: "assets",
    title: "素材与证明",
    hint: "素材从哪来、要交的证明怎么出",
    items: [
      {
        key: "library",
        path: "/library",
        label: "长剧素材",
        subtitle: "逐集导入要切的长剧，自动探测时长、分辨率与音轨",
      },
      {
        key: "asset",
        path: "/asset",
        label: "自备素材",
        subtitle: "导入你自己的音效 / BGM / LUT，不内置任何第三方素材",
      },
      {
        key: "proof",
        path: "/proof",
        label: "证明材料",
        subtitle: "模板图填数据，一键出可直接提交的证明图",
      },
      {
        key: "home",
        path: "/projects",
        label: "项目",
        subtitle: "一个项目 = 一部短剧素材 + 它的切片与分发记录",
      },
    ],
  },
  {
    key: "system",
    title: "系统",
    hint: "看进度、改路径",
    items: [
      {
        key: "queue",
        path: "/queue",
        label: "任务队列",
        subtitle: "把长任务排成一队顺序跑，关掉应用再打开会接着跑",
      },
      {
        key: "settings",
        path: "/settings",
        label: "设置",
        subtitle: "指定 ffmpeg / ffprobe / Python 路径，查看各组件可用性",
      },
    ],
  },
];

/** 拍平后的全部条目，供「按路径查标题」用。 */
export const navItems: NavItem[] = [
  primaryNav,
  ...navGroups.flatMap((g) => g.items),
];

export function navByPath(path: string): NavItem | undefined {
  return navItems.find((n) => n.path === path);
}

/** 某个路径属于哪个分组（首页要按分组显示入口）。 */
export function groupOf(path: string): NavGroup | undefined {
  return navGroups.find((g) => g.items.some((i) => i.path === path));
}
