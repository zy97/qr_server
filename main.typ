#set text(weight: "bold", font: "Microsoft YaHei UI")


#set page(
  margin: 2pt,
)
#let title = "廊坊舒畅汽车零部件有限公司"

#let data = json("data.json");
#let qr_string = data.qr_string
#let parts = qr_string.split("|")
#let material_no = parts.at(0)
#let serial_no = parts.at(1)
#let order_no = parts.at(2)
#let count = parts.at(3)
#let date = parts.at(5)
#let box_no = parts.at(6)

#let part_no = data.part_no
#let material_name = data.material_name
#let customer_name = data.customer_name

#set page(width: 15cm, height: auto)
#import "@preview/tiaoma:0.3.0"

#table(
  columns: (75pt, 1fr, 50pt, 60pt, 100pt),
  align: center,
  inset: 7pt,
  table.cell(colspan: 5, align: horizon)[
    #grid(
      columns: (80pt, 1fr),
      grid.cell(
        image("logo.png", width: 40%),
        align: right,
      ),
      grid.cell(
        text(title, size: 20pt),
        align: left,
      ),
    )
  ],

  "零件号",
  table.cell(colspan: 2, part_no),
  "物料编码",
  material_no,

  "物料名称",
  table.cell(colspan: 3, material_name),
  box_no,
  table.hline(start: 4, stroke: 0pt),

  "生成订单号",
  order_no,
  "数量",
  count,
  table.cell(rowspan: 3, tiaoma.qrcode(qr_string, width: 2cm)),

  "批次号",
  serial_no,
  "检验员",
  "",

  "生产日期",
  table.cell(colspan: 3, align: center, date),

  "客户名称",
  table.cell(colspan: 4, align: center, customer_name),
)
