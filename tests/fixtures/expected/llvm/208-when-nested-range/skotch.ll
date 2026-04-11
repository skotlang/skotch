; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [2 x i8] c"A\00", align 1
@.str.1 = private unnamed_addr constant [2 x i8] c"B\00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"C\00", align 1
@.str.3 = private unnamed_addr constant [2 x i8] c"D\00", align 1
@.str.4 = private unnamed_addr constant [2 x i8] c"F\00", align 1
@.str.5 = private unnamed_addr constant [8 x i8] c"Invalid\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_category(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  br label %bb1
bb1:
  %t0 = add i32 0, 90
  %t1 = add i32 0, 100
  %t2 = icmp sge i32 %arg0, %t0
  %t3 = icmp sle i32 %arg0, %t1
  %t4 = zext i1 %t2 to i32
  %t5 = zext i1 %t3 to i32
  %t6 = mul i32 %t4, %t5
  %t7 = trunc i32 %t6 to i1
  br i1 %t7, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb12
bb3:
  %t8 = add i32 0, 80
  %t9 = add i32 0, 89
  %t10 = icmp sge i32 %arg0, %t8
  %t11 = icmp sle i32 %arg0, %t9
  %t12 = zext i1 %t10 to i32
  %t13 = zext i1 %t11 to i32
  %t14 = mul i32 %t12, %t13
  %t15 = trunc i32 %t14 to i1
  br i1 %t15, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb12
bb5:
  %t16 = add i32 0, 70
  %t17 = add i32 0, 79
  %t18 = icmp sge i32 %arg0, %t16
  %t19 = icmp sle i32 %arg0, %t17
  %t20 = zext i1 %t18 to i32
  %t21 = zext i1 %t19 to i32
  %t22 = mul i32 %t20, %t21
  %t23 = trunc i32 %t22 to i1
  br i1 %t23, label %bb6, label %bb7
bb6:
  store ptr @.str.2, ptr %merge_2
  br label %bb12
bb7:
  %t24 = add i32 0, 60
  %t25 = add i32 0, 69
  %t26 = icmp sge i32 %arg0, %t24
  %t27 = icmp sle i32 %arg0, %t25
  %t28 = zext i1 %t26 to i32
  %t29 = zext i1 %t27 to i32
  %t30 = mul i32 %t28, %t29
  %t31 = trunc i32 %t30 to i1
  br i1 %t31, label %bb8, label %bb9
bb8:
  store ptr @.str.3, ptr %merge_2
  br label %bb12
bb9:
  %t32 = add i32 0, 0
  %t33 = add i32 0, 59
  %t34 = icmp sge i32 %arg0, %t32
  %t35 = icmp sle i32 %arg0, %t33
  %t36 = zext i1 %t34 to i32
  %t37 = zext i1 %t35 to i32
  %t38 = mul i32 %t36, %t37
  %t39 = trunc i32 %t38 to i1
  br i1 %t39, label %bb10, label %bb11
bb10:
  store ptr @.str.4, ptr %merge_2
  br label %bb12
bb11:
  store ptr @.str.5, ptr %merge_2
  br label %bb12
bb12:
  %t40 = load ptr, ptr %merge_2
  ret ptr %t40
}

define i32 @main() {
entry:
  %t0 = add i32 0, 95
  %t1 = call ptr @InputKt_category(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, 85
  %t4 = call ptr @InputKt_category(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 72
  %t7 = call ptr @InputKt_category(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 65
  %t10 = call ptr @InputKt_category(i32 %t9)
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 45
  %t13 = call ptr @InputKt_category(i32 %t12)
  call i32 @puts(ptr %t13)
  %t15 = add i32 0, -1
  %t16 = call ptr @InputKt_category(i32 %t15)
  call i32 @puts(ptr %t16)
  ret i32 0
}

