export type AgentActivity =
  | 'idle'
  | 'walking'
  | 'coding'
  | 'thinking'
  | 'water_break'
  | 'coffee_break'
  | 'lunch'
  | 'meeting'
  | 'chatting'
  | 'celebrating'
  | 'frustrated'

export type Expression =
  | 'neutral'
  | 'happy'
  | 'focused'
  | 'confused'
  | 'tired'
  | 'excited'

export type Point = {
  x: number
  y: number
}

export type AgentBehaviorState = {
  activity: AgentActivity
  position: Point
  targetPosition: Point
  deskPosition: Point
  expression: Expression
  chatMessage: string | null
  chatTarget: string | null
  lastBreak: number
  breakInterval: number
  activityStartTime: number
}

export const LOCATIONS = {
  waterCooler: { x: 5, y: 45 },
  coffeeMachine: { x: 90, y: 42 },
  lunchArea: { x: 88, y: 85 },
  meetingTable: { x: 45, y: 52 },
} as const

export const DESK_POSITIONS = [
  { x: 18, y: 28, deskX: 18, deskY: 18 },
  { x: 42, y: 28, deskX: 42, deskY: 18 },
  { x: 66, y: 28, deskX: 66, deskY: 18 },
  { x: 18, y: 55, deskX: 18, deskY: 45 },
  { x: 42, y: 55, deskX: 42, deskY: 45 },
  { x: 66, y: 55, deskX: 66, deskY: 45 },
  { x: 30, y: 78, deskX: 30, deskY: 68 },
  { x: 55, y: 78, deskX: 55, deskY: 68 },
] as const

export const EXPRESSION_MAP: Record<AgentActivity, Expression> = {
  idle: 'neutral',
  walking: 'neutral',
  coding: 'focused',
  thinking: 'confused',
  water_break: 'tired',
  coffee_break: 'tired',
  lunch: 'happy',
  meeting: 'neutral',
  chatting: 'happy',
  celebrating: 'excited',
  frustrated: 'confused',
}

export const CHAT_MESSAGES = {
  working: [
    'Almost done...',
    'This is interesting',
    'Compiling...',
    'Reading docs...',
    'Found a bug!',
    'Writing tests...',
    'Pushing code...',
    'Reviewing PR...',
  ],
  break: [
    'Need water 💧',
    'brb',
    'Quick break',
    'Coffee time ☕',
    'Need caffeine',
    'Lunch break 🍕',
  ],
  chatting: [
    'Hey {name}!',
    'Check this out',
    'Can you review?',
    'Nice work!',
    'Need your help',
    'Lets sync up',
    'What do you think?',
    'Almost there!',
  ],
  complete: [
    'Done! 🎉',
    'Ship it!',
    'All green ✅',
    'Nailed it!',
    'Task complete!',
  ],
  failed: [
    'Hmm...',
    'Thats broken',
    'Need help...',
    'Something went wrong',
    'Debugging...',
  ],
} as const

export function getExpression(activity: AgentActivity): Expression {
  return EXPRESSION_MAP[activity]
}

export function getRandomMessage(pool: keyof typeof CHAT_MESSAGES): string {
  const messages = CHAT_MESSAGES[pool]
  const index = Math.floor(Math.random() * messages.length)
  return messages[index]
}

export function getBreakType(): AgentActivity {
  const breakTypes: Array<AgentActivity> = [
    'water_break',
    'coffee_break',
    'lunch',
    'meeting',
  ]
  const index = Math.floor(Math.random() * breakTypes.length)
  return breakTypes[index]
}

export function getLocationForActivity(
  activity: AgentActivity,
  deskIndex: number,
): Point {
  const safeDeskIndex =
    ((deskIndex % DESK_POSITIONS.length) + DESK_POSITIONS.length) %
    DESK_POSITIONS.length
  const desk = DESK_POSITIONS[safeDeskIndex]

  if (activity === 'water_break') {
    return { ...LOCATIONS.waterCooler }
  }
  if (activity === 'coffee_break') {
    return { ...LOCATIONS.coffeeMachine }
  }
  if (activity === 'lunch') {
    return { ...LOCATIONS.lunchArea }
  }
  if (activity === 'meeting') {
    return { ...LOCATIONS.meetingTable }
  }
  if (activity === 'chatting' || activity === 'celebrating') {
    const angle = ((safeDeskIndex * 45) % 360) * (Math.PI / 180)
    return {
      x: LOCATIONS.meetingTable.x + Math.cos(angle) * 6,
      y: LOCATIONS.meetingTable.y + Math.sin(angle) * 4,
    }
  }

  return { x: desk.x, y: desk.y }
}

export function lerpPosition(
  current: Point,
  target: Point,
  speed = 0.03,
): Point {
  return {
    x: current.x + (target.x - current.x) * speed,
    y: current.y + (target.y - current.y) * speed,
  }
}

export function isAtTarget(
  current: Point,
  target: Point,
  threshold = 1.5,
): boolean {
  const deltaX = current.x - target.x
  const deltaY = current.y - target.y
  const distance = Math.sqrt(deltaX * deltaX + deltaY * deltaY)
  return distance <= threshold
}

export function createBehaviorState(deskIndex: number): AgentBehaviorState {
  const timestamp = Date.now()
  const safeDeskIndex =
    ((deskIndex % DESK_POSITIONS.length) + DESK_POSITIONS.length) %
    DESK_POSITIONS.length
  const desk = DESK_POSITIONS[safeDeskIndex]
  const deskPosition = { x: desk.x, y: desk.y }

  return {
    activity: 'idle',
    position: deskPosition,
    targetPosition: deskPosition,
    deskPosition,
    expression: getExpression('idle'),
    chatMessage: null,
    chatTarget: null,
    lastBreak: timestamp,
    breakInterval: 90_000 + Math.floor(Math.random() * 120_000),
    activityStartTime: timestamp,
  }
}
