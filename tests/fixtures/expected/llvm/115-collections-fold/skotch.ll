; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = add i32 0, 2
  %t2 = add i32 0, 3
  %t3 = add i32 0, 4
  %t4 = add i32 0, 5
  %t5 = add i32 0, 5
  %t6 = add i32 0, 0
  %t7 = inttoptr i64 0 to ptr
  %t8 = add i32 0, 1
  %t9 = inttoptr i64 0 to ptr
  %t10 = add i32 0, 2
  %t11 = inttoptr i64 0 to ptr
  %t12 = add i32 0, 3
  %t13 = inttoptr i64 0 to ptr
  %t14 = add i32 0, 4
  %t15 = inttoptr i64 0 to ptr
  %t16 = inttoptr i64 0 to ptr
  %t17 = add i32 0, 0
  %t18 = inttoptr i64 0 to ptr
  %t19 = inttoptr i64 0 to ptr
  %t20 = inttoptr i64 0 to ptr
  call i32 @puts(ptr %t20)
  ret i32 0
}

