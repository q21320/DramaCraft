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
  /** 菜单里不显示（路由保留，直链/旧书签仍能用） */
  hidden?: boolean;
}

export interface NavGroup {
  key: string;
  title: string;
  /** 一句话说明这组什么时候用得上 */
  hint: string;
  /** 整组不在菜单里显示 */
  hidden?: boolean;
  items: NavItem[];
}


export const videoNav: NavItem = {
  key: "video",
  path: "/video",
  label: "视频",
  subtitle: "本地视频与在线短剧",
};

/** 主入口。直接摆在菜单最上面，不藏在任何分组里。 */
export const primaryNav: NavItem = {
  key: "pipeline",
  path: "/pipeline",
  label: "整剧处理",
  subtitle: "指一个源目录，从头到尾把一批剧做完（一部做完再做下一部）",
};

/**
 * 其余按用途分组，**默认全部收起**。
 *
 * 顺序按「从流水线出来之后最可能想干什么」排，不按实现顺序排。
 *
 * 用户确认「单步工具」「素材与证明」两组都用不上、任务队列也不用看 ——
 * 整组/整项标记 `hidden`，菜单不再渲染；路由保留，页面本身不删。
 */
const allNavGroups: NavGroup[] = [
  {
    key: "tools",
    title: "单步工具",
    hint: "只想处理某一件小事时用（整剧处理里已经自动做了这些）",
    hidden: true,
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
    hidden: true,
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
        key: "logs",
        path: "/logs",
        label: "运行日志",
        subtitle: "每一步的过程：扫到什么、导入了多少、哪一步成没成；也能看产出",
      },
      {
        key: "queue",
        path: "/queue",
        label: "任务队列",
        subtitle: "把长任务排成一队顺序跑，关掉应用再打开会接着跑",
        hidden: true,
      },
      {
        key: "ai",
        path: "/ai",
        label: "AI 服务",
        subtitle: "API Key、接口地址、模型名与云端字幕",
      },
      {
        key: "settings",
        path: "/settings",
        label: "设置",
        subtitle: "下载目录、成本报告落款（外部工具等归入高级设置）",
      },
    ],
  },
];

/** 菜单实际渲染的分组：去掉 hidden 的组和项。 */
export const navGroups: NavGroup[] = allNavGroups
  .filter((g) => !g.hidden)
  .map((g) => ({ ...g, items: g.items.filter((i) => !i.hidden) }));

/**
 * 拍平后的全部条目，供「按路径查标题」用。
 *
 * **用 allNavGroups 而不是 navGroups** —— 隐藏页的页头标题也要对，
 * 否则直接打开 /queue 会顶着「流水线」的名字。
 */
export const navItems: NavItem[] = [
  videoNav,
  primaryNav,
  ...allNavGroups.flatMap((g) => g.items),
];

export function navByPath(path: string): NavItem | undefined {
  // 子页面（例如 /video/:seriesId/:vid）也算在父级入口下，页头文案才不会串到流水线
  return navItems.find((n) => n.path === path || path.startsWith(`${n.path}/`));
}

/** 某个路径属于哪个分组（首页要按分组显示入口）。 */
export function groupOf(path: string): NavGroup | undefined {
  return allNavGroups.find((g) => g.items.some((i) => i.path === path));
}
